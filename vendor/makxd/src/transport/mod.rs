pub(crate) mod encryption;
#[cfg(feature = "mock")]
pub mod mock;
pub(crate) mod monitor;
pub(crate) mod reader;
pub(crate) mod serial;
#[cfg(windows)]
pub(crate) mod windows_serial;
pub(crate) mod wire;
pub(crate) mod writer;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel as channel;

use crate::error::{MakxdError, Result};
use crate::protocol::api::{ApiOpcode, framed_command};
use crate::types::{
    ButtonMask, ConnectionConfig, ConnectionState, DeviceKinds, device_kinds_parse,
};

use self::encryption::TransportEncryption;
use self::reader::ReaderSignal;
use self::writer::WriteRequest;

pub(crate) struct PendingResponse {
    pub id: u64,
    pub response_tx: channel::Sender<Vec<u8>>,
    pub expected_nonce: Option<[u8; 12]>,
    pub expected_opcode: Option<u8>,
}

// ---------------------------------------------------------------------------
// TransportHandle — public(crate) API surface
// ---------------------------------------------------------------------------

/// Handle to the transport layer. Cheaply cloneable (wraps Arc).
#[derive(Clone)]
pub(crate) struct TransportHandle {
    inner: Arc<TransportInner>,
}

/// Shared interior — one per connection, referenced by handle + monitor.
pub(crate) struct TransportInner {
    pub conn_state: AtomicU8,
    pub shutdown: AtomicBool,
    pub port_name: Mutex<String>,
    device_kinds: Mutex<Option<DeviceKinds>>,
    settings_lock: Mutex<()>,

    // Channel for sending commands to the writer thread.
    // Wrapped in Mutex<Option<>> so shutdown() can drop the sender to unblock
    // the writer thread (which blocks on recv).
    write_tx: Mutex<Option<channel::Sender<WriteRequest>>>,
    // Receiver clone for spawning new writer threads on reconnect.
    write_rx: channel::Receiver<WriteRequest>,

    // Pending response oneshots: writer pushes, reader pops. Shared Arc.
    pending_responses: Arc<Mutex<VecDeque<PendingResponse>>>,
    next_response_id: AtomicU64,
    // Only one request/response transaction is in flight. A late response
    // can therefore never be matched to a newer query after a timeout.
    query_lock: Mutex<()>,
    pub transport_encryption: Option<Arc<TransportEncryption>>,

    // Button event subscribers.
    button_subs: Arc<Mutex<Vec<channel::Sender<ButtonMask>>>>,
    change_subs: Arc<Mutex<Vec<channel::Sender<crate::stream::InputChange>>>>,

    // Connection state subscribers.
    pub state_subs: Mutex<Vec<channel::Sender<ConnectionState>>>,

    // Reader signal for disconnect notification (replaced on reconnect).
    pub reader_signal: Mutex<Option<Arc<ReaderSignal>>>,

    // Thread handles (for join on shutdown).
    threads: Mutex<Vec<JoinHandle<()>>>,
    monitor_thread: Mutex<Option<JoinHandle<()>>>,

    // Reconnection backoff (initial value; doubles up to 5s).
    pub reconnect_backoff: Duration,
    pub connection: ConnectionConfig,
}

impl TransportInner {
    /// Spawn fresh reader + writer threads for the given port.
    /// Used both on initial connect and on reconnect.
    pub fn spawn_io_threads(&self, port: Box<dyn wire::WirePort>) -> Result<()> {
        let reader_port = port.try_clone_wire()?;
        let writer_port = port;

        let signal = Arc::new(ReaderSignal::new());

        // Spawn writer.
        let write_rx = self.write_rx.clone();
        let pending = Arc::clone(&self.pending_responses);
        let writer_signal = Arc::clone(&signal);
        let writer_handle = std::thread::Builder::new()
            .name("makxd-writer".into())
            .spawn(move || {
                writer::writer_thread(writer_port, write_rx, pending, writer_signal);
            })
            .map_err(MakxdError::Io)?;

        // Spawn reader.
        let reader_pending = Arc::clone(&self.pending_responses);
        let reader_buttons = Arc::clone(&self.button_subs);
        let reader_changes = Arc::clone(&self.change_subs);
        let reader_signal = Arc::clone(&signal);
        let reader_encryption = self.transport_encryption.clone();
        let reader_handle = std::thread::Builder::new()
            .name("makxd-reader".into())
            .spawn(move || {
                reader::reader_thread(
                    reader_port,
                    reader_pending,
                    reader_buttons,
                    reader_changes,
                    reader_signal,
                    reader_encryption,
                );
            })
            .map_err(MakxdError::Io)?;

        // Store signal for monitor to wait on.
        *self.reader_signal.lock().unwrap() = Some(signal);

        // Drain old thread handles (they've exited by now on reconnect —
        // old reader exits on port read error, old writer exits on port
        // write error). Join them to reclaim resources.
        let mut threads = self.threads.lock().unwrap();
        let old: Vec<_> = threads.drain(..).collect();
        drop(threads);
        for handle in old {
            let _ = handle.join();
        }

        // Store new thread handles.
        let mut threads = self.threads.lock().unwrap();
        threads.push(reader_handle);
        threads.push(writer_handle);

        Ok(())
    }

    /// Send a payload through the write channel. Returns Disconnected if the
    /// channel has been shut down.
    fn send_payload(&self, payload: WriteRequest) -> Result<()> {
        let guard = self.write_tx.lock().unwrap();
        if let Some(tx) = guard.as_ref() {
            tx.send(payload).map_err(|_| MakxdError::Disconnected)
        } else {
            Err(MakxdError::Disconnected)
        }
    }

    pub fn notify_state(&self, state: ConnectionState) {
        let mut subs = self.state_subs.lock().unwrap();
        subs.retain(|sub| sub.send(state).is_ok());
    }
}

impl TransportHandle {
    /// Connect to the device and spawn I/O threads.
    pub fn connect(
        connection: ConnectionConfig,
        reconnect: bool,
        reconnect_backoff: Duration,
        transport_encryption: Option<TransportEncryption>,
    ) -> Result<Self> {
        let transport_encryption = transport_encryption.map(Arc::new);
        let (port, probed_kinds, port_name) = match &connection {
            ConnectionConfig::Com {
                port: Some(port_name),
            } => {
                let (port, kinds) =
                    serial::establish_connection(port_name, transport_encryption.as_deref())?;
                (port, kinds, port_name.clone())
            }
            ConnectionConfig::Com { port: None } => {
                return Err(MakxdError::Protocol("resolved COM port is required".into()));
            }
            ConnectionConfig::Udp {
                host,
                port: udp_port,
                ..
            } => {
                let (port, kinds) = wire::establish_network_connection(
                    &connection,
                    transport_encryption.as_deref(),
                )?;
                (port, kinds, format!("udp://{host}:{udp_port}"))
            }
            ConnectionConfig::Ble { address, .. } => {
                let (port, kinds) = wire::establish_network_connection(
                    &connection,
                    transport_encryption.as_deref(),
                )?;
                (port, kinds, format!("ble://{address}"))
            }
        };
        // Keep memory bounded when a device or driver stalls. Synchronous SET
        // calls wait for writer completion, so normal callers apply backpressure.
        let (write_tx, write_rx) = channel::bounded::<WriteRequest>(64);

        let inner = Arc::new(TransportInner {
            settings_lock: Mutex::new(()),
            conn_state: AtomicU8::new(ConnectionState::Connected as u8),
            shutdown: AtomicBool::new(false),
            port_name: Mutex::new(port_name),
            device_kinds: Mutex::new(None),
            write_tx: Mutex::new(Some(write_tx)),
            write_rx,
            pending_responses: Arc::new(Mutex::new(VecDeque::new())),
            next_response_id: AtomicU64::new(1),
            query_lock: Mutex::new(()),
            transport_encryption,
            button_subs: Arc::new(Mutex::new(Vec::new())),
            change_subs: Arc::new(Mutex::new(Vec::new())),
            state_subs: Mutex::new(Vec::new()),
            reader_signal: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
            monitor_thread: Mutex::new(None),
            reconnect_backoff,
            connection,
        });

        inner.spawn_io_threads(port)?;
        let handle = Self { inner };
        *handle.inner.device_kinds.lock().unwrap() = Some(DeviceKinds {
            kinds: probed_kinds,
        });

        // Spawn monitor if reconnection is enabled.
        if reconnect {
            let monitor_inner = Arc::clone(&handle.inner);
            let monitor = std::thread::Builder::new()
                .name("makxd-monitor".into())
                .spawn(move || {
                    monitor::monitor_thread(monitor_inner);
                })
                .map_err(MakxdError::Io)?;
            *handle.inner.monitor_thread.lock().unwrap() = Some(monitor);
        }

        Ok(handle)
    }

    /// Create a handle wrapping a mock transport.
    /// Spawns a mock worker thread that routes commands through MockTransport.
    #[cfg(feature = "mock")]
    pub fn from_mock() -> (Self, Arc<mock::MockTransport>) {
        let (write_tx, write_rx) = channel::bounded::<WriteRequest>(64);
        let mock_transport = Arc::new(mock::MockTransport::new());
        let button_subs = Arc::new(Mutex::new(Vec::new()));

        let inner = Arc::new(TransportInner {
            settings_lock: Mutex::new(()),
            conn_state: AtomicU8::new(ConnectionState::Connected as u8),
            shutdown: AtomicBool::new(false),
            port_name: Mutex::new("mock".into()),
            device_kinds: Mutex::new(Some(DeviceKinds { kinds: 0x07 })),
            write_tx: Mutex::new(Some(write_tx)),
            write_rx: write_rx.clone(),
            pending_responses: Arc::new(Mutex::new(VecDeque::new())),
            next_response_id: AtomicU64::new(1),
            query_lock: Mutex::new(()),
            transport_encryption: None,
            button_subs: Arc::clone(&button_subs),
            change_subs: Arc::new(Mutex::new(Vec::new())),
            state_subs: Mutex::new(Vec::new()),
            reader_signal: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
            monitor_thread: Mutex::new(None),
            reconnect_backoff: Duration::from_millis(100),
            connection: ConnectionConfig::com(None),
        });

        // Spawn mock worker thread that processes commands through MockTransport.
        let mock_clone = Arc::clone(&mock_transport);
        let subs_clone = Arc::clone(&button_subs);
        let handle = std::thread::Builder::new()
            .name("makxd-mock-worker".into())
            .spawn(move || {
                mock::mock_worker(write_rx, mock_clone, subs_clone);
            })
            .unwrap();

        inner.threads.lock().unwrap().push(handle);

        (Self { inner }, mock_transport)
    }

    pub fn send_mak_api(
        &self,
        opcode: ApiOpcode,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let _query = self
            .inner
            .query_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.send_framed_submit(opcode as u8, payload, false, timeout)
    }
    pub(crate) fn settings_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.inner
            .settings_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn device_kinds(&self, timeout: Duration) -> Result<DeviceKinds> {
        let mut device_kinds = self.inner.device_kinds.lock().unwrap();
        if let Some(info) = *device_kinds {
            return Ok(info);
        }
        let learned = device_kinds_parse(&self.send_mak_api(ApiOpcode::Device, &[], timeout)?)?;
        *device_kinds = Some(learned);
        Ok(learned)
    }

    pub fn write_mak_api(
        &self,
        opcode: ApiOpcode,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        self.write_framed(opcode as u8, payload, timeout)?;
        Ok(())
    }

    pub(crate) fn write_framed(
        &self,
        command: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        self.send_framed_submit(command, payload, true, timeout)?;
        Ok(())
    }

    fn send_framed_submit(
        &self,
        command: u8,
        payload: &[u8],
        fire_and_forget: bool,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        if !self.is_connected() {
            return Err(MakxdError::Disconnected);
        }
        let (data, expected_nonce) =
            if let Some(encryption) = self.inner.transport_encryption.as_deref() {
                let mut record = Vec::with_capacity(1 + payload.len());
                record.push(command);
                record.extend_from_slice(payload);
                let (frame, nonce) = encryption.encode_command(&record)?;
                (frame, Some(nonce))
            } else {
                (framed_command(command, payload)?, None)
            };
        if fire_and_forget {
            let (completion_tx, completion_rx) = channel::bounded(1);
            self.inner.send_payload(WriteRequest::Command {
                data,
                response_tx: None,
                write_completion: Some(completion_tx),
                response_id: None,
                expected_nonce,
                expected_opcode: Some(command),
            })?;
            return match completion_rx.recv_timeout(timeout) {
                Ok(Ok(())) => Ok(Vec::new()),
                Ok(Err(message)) => Err(MakxdError::Io(std::io::Error::other(message))),
                Err(channel::RecvTimeoutError::Timeout) => Err(MakxdError::Timeout),
                Err(channel::RecvTimeoutError::Disconnected) => Err(MakxdError::Disconnected),
            };
        }
        let (tx, rx) = channel::bounded(1);
        let response_id = self.inner.next_response_id.fetch_add(1, Ordering::Relaxed);
        self.inner.send_payload(WriteRequest::Command {
            data,
            response_tx: Some(tx),
            write_completion: None,
            response_id: Some(response_id),
            expected_nonce,
                expected_opcode: Some(command),
        })?;
        match rx.recv_timeout(timeout) {
            Ok(response) if response.len() == 2 && response[1] == 0xff => Err(
                MakxdError::Protocol(format!("framed command 0x{:02x} was rejected", response[0])),
            ),
            Ok(response) if response.len() >= 2 => Ok(response[1..].to_vec()),
            Ok(_) => Err(MakxdError::Protocol("MAK_API response is malformed".into())),
            Err(channel::RecvTimeoutError::Timeout) => {
                self.inner
                    .pending_responses
                    .lock()
                    .unwrap()
                    .retain(|pending| pending.id != response_id);
                Err(MakxdError::Timeout)
            }
            Err(channel::RecvTimeoutError::Disconnected) => Err(MakxdError::Disconnected),
        }
    }

    #[cfg(feature = "async")]
    pub async fn send_mak_api_async(
        &self,
        opcode: ApiOpcode,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let handle = self.clone();
        let payload = payload.to_vec();
        tokio::task::spawn_blocking(move || handle.send_mak_api(opcode, &payload, timeout))
            .await
            .map_err(|error| MakxdError::Protocol(format!("tokio join error: {error}")))?
    }

    #[cfg(feature = "async")]
    pub async fn write_mak_api_async(
        &self,
        opcode: ApiOpcode,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        let handle = self.clone();
        let payload = payload.to_vec();
        tokio::task::spawn_blocking(move || handle.write_mak_api(opcode, &payload, timeout))
            .await
            .map_err(|error| MakxdError::Protocol(format!("tokio join error: {error}")))?
    }

    #[cfg(feature = "async")]
    pub(crate) async fn write_framed_async(
        &self,
        command: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        let handle = self.clone();
        let payload = payload.to_vec();
        tokio::task::spawn_blocking(move || handle.write_framed(command, &payload, timeout))
            .await
            .map_err(|error| MakxdError::Protocol(format!("tokio join error: {error}")))?
    }

    pub fn drain(&self, timeout: Duration) -> Result<()> {
        let (completion, response) = channel::bounded(1);
        self.inner.send_payload(WriteRequest::Drain {
            completion,
            timeout,
        })?;
        match response.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(MakxdError::Io(std::io::Error::other(message))),
            Err(channel::RecvTimeoutError::Timeout) => Err(MakxdError::Timeout),
            Err(channel::RecvTimeoutError::Disconnected) => Err(MakxdError::Disconnected),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connection_state() == ConnectionState::Connected
    }

    pub fn connection_state(&self) -> ConnectionState {
        ConnectionState::from_u8(self.inner.conn_state.load(Ordering::Acquire))
    }

    /// Get the port name this transport is connected to.
    pub fn port_name(&self) -> String {
        self.inner.port_name.lock().unwrap().clone()
    }

    /// Subscribe to connection state changes.
    pub fn subscribe_state(&self) -> channel::Receiver<ConnectionState> {
        let (tx, rx) = channel::unbounded();
        self.inner.state_subs.lock().unwrap().push(tx);
        rx
    }

    /// Subscribe to button events from the device stream.
    pub fn subscribe_input_changes(&self) -> channel::Receiver<crate::stream::InputChange> {
        let (tx, rx) = channel::unbounded();
        self.inner.change_subs.lock().unwrap().push(tx);
        rx
    }

    pub fn subscribe_buttons(&self) -> channel::Receiver<ButtonMask> {
        let (tx, rx) = channel::unbounded();
        self.inner.button_subs.lock().unwrap().push(tx);
        rx
    }

    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Release);
        self.inner
            .conn_state
            .store(ConnectionState::Disconnected as u8, Ordering::Release);

        // Drop the write channel sender to unblock the writer thread.
        *self.inner.write_tx.lock().unwrap() = None;

        // Signal reader thread to exit (it checks this on timeout).
        // Wake monitor if waiting.
        if let Some(sig) = self.inner.reader_signal.lock().unwrap().as_ref() {
            sig.signal_disconnect();
        }

        // Note: we do NOT join threads here — the reader thread will exit
        // on its next port read timeout (max 200ms). The writer
        // thread will exit because we dropped write_tx above. The reader
        // thread will exit on its next port read error or timeout. Thread
        // handles are cleaned up when TransportInner is dropped.
    }

    /// Stop I/O and wait until the reader and writer have released their
    /// carrier handles. This is intended for callers that will reopen a COM
    /// port immediately afterwards.
    pub fn shutdown_and_wait(&self, timeout: Duration) -> Result<()> {
        self.shutdown();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let finished = {
                let io_done = self
                    .inner
                    .threads
                    .lock()
                    .unwrap()
                    .iter()
                    .all(JoinHandle::is_finished);
                let monitor_done = self
                    .inner
                    .monitor_thread
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_none_or(JoinHandle::is_finished);
                io_done && monitor_done
            };
            if finished {
                let handles: Vec<_> = self.inner.threads.lock().unwrap().drain(..).collect();
                for handle in handles {
                    let _ = handle.join();
                }
                if let Some(handle) = self.inner.monitor_thread.lock().unwrap().take() {
                    let _ = handle.join();
                }
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(MakxdError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for TransportInner {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Drop the write channel sender.
        *self.write_tx.lock().unwrap() = None;
        if let Some(sig) = self.reader_signal.lock().unwrap().as_ref() {
            let (lock, cvar) = &sig.disconnect_notify;
            let mut d = lock.lock().unwrap();
            *d = true;
            cvar.notify_all();
        }
        if let ConnectionConfig::Ble { io, .. } = &self.connection {
            io.close();
        }
    }
}
