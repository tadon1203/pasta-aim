use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel as channel;

use super::PendingResponse;
use super::reader::ReaderSignal;
use super::wire::WirePort;

static COMMAND_TRACE: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_command_trace(enabled: bool) {
    COMMAND_TRACE.store(enabled, Ordering::Release);
}

pub(crate) enum WriteRequest {
    Command {
        data: Vec<u8>,
        response_tx: Option<channel::Sender<Vec<u8>>>,
        response_id: Option<u64>,
        /// SET completion means the OS write operation completed, not a drain.
        write_completion: Option<channel::Sender<std::result::Result<(), String>>>,
        expected_nonce: Option<[u8; 12]>,
        expected_opcode: Option<u8>,
    },
    Drain {
        completion: channel::Sender<std::result::Result<(), String>>,
        timeout: Duration,
    },
}

pub(crate) fn writer_thread(
    mut port: Box<dyn WirePort>,
    rx: channel::Receiver<WriteRequest>,
    pending_responses: Arc<Mutex<VecDeque<PendingResponse>>>,
    signal: Arc<ReaderSignal>,
) {
    let mut queued = Vec::with_capacity(64);
    let mut responses: Vec<PendingResponse> = Vec::new();

    'outer: loop {
        let request = match rx.recv() {
            Ok(p) => p,
            Err(_) => return,
        };

        if let WriteRequest::Drain {
            completion,
            timeout,
        } = request
        {
            let started = Instant::now();
            let result = port.drain_wire(timeout).map_err(|error| error.to_string());
            if COMMAND_TRACE.load(Ordering::Acquire) {
                let elapsed = started.elapsed();
                tracing::trace!(
                    drain_us = elapsed.as_micros(),
                    drain_ms = elapsed.as_secs_f64() * 1_000.0,
                    "makxd wire drain completed"
                );
            }
            let _ = completion.send(result);
            continue;
        }

        queued.clear();
        responses.clear();
        let mut completions = Vec::new();
        let mut response_expected = false;
        let mut add_command = |request: WriteRequest| {
            let WriteRequest::Command {
                data,
                response_tx,
                response_id,
                write_completion,
                expected_nonce,
                expected_opcode,
            } = request
            else {
                unreachable!()
            };
            response_expected |= response_tx.is_some();
            queued.push(data);
            if let Some(tx) = write_completion {
                completions.push(tx);
            }
            if let Some(tx) = response_tx {
                responses.push(PendingResponse {
                    id: response_id.unwrap_or_default(),
                    response_tx: tx,
                    expected_nonce,
                    expected_opcode,
                });
            }
        };
        add_command(request);
        // Stop collecting at Drain. It remains queued and is processed after this batch.
        if port.write_coalescing_supported() {
            while let Ok(next) = rx.try_recv() {
                match next {
                    command @ WriteRequest::Command { .. } => add_command(command),
                    drain @ WriteRequest::Drain { .. } => {
                        // Execute this drain immediately after the preceding command batch.
                        drop(add_command);
                        if !responses.is_empty() {
                            let mut pending = pending_responses.lock().unwrap();
                            pending.extend(responses.drain(..));
                        }
                        let bytes = queued.iter().map(Vec::len).sum::<usize>();
                        let started = Instant::now();
                        if let Err(error) = port.write_queued_wire(&queued, response_expected) {
                            let message = error.to_string();
                            for tx in completions.drain(..) {
                                let _ = tx.send(Err(message.clone()));
                            }
                            signal.signal_disconnect();
                            return;
                        }
                        if COMMAND_TRACE.load(Ordering::Acquire) {
                            let elapsed = started.elapsed();
                            tracing::trace!(
                                records = queued.len(),
                                bytes,
                                write_us = elapsed.as_micros(),
                                write_ms = elapsed.as_secs_f64() * 1_000.0,
                                "makxd wire write completed"
                            );
                        }
                        for tx in completions.drain(..) {
                            let _ = tx.send(Ok(()));
                        }
                        let WriteRequest::Drain {
                            completion,
                            timeout,
                        } = drain
                        else {
                            unreachable!()
                        };
                        let started = Instant::now();
                        let result = port.drain_wire(timeout).map_err(|error| error.to_string());
                        if COMMAND_TRACE.load(Ordering::Acquire) {
                            let elapsed = started.elapsed();
                            tracing::trace!(
                                drain_us = elapsed.as_micros(),
                                drain_ms = elapsed.as_secs_f64() * 1_000.0,
                                "makxd wire drain completed"
                            );
                        }
                        let _ = completion.send(result);
                        continue 'outer;
                    }
                }
            }
        }

        // Register response receivers BEFORE writing at the runtime baud
        // can respond before write_all returns, and the reader must already
        // have the sender in the queue to deliver the response.
        if !responses.is_empty() {
            let mut pending = pending_responses.lock().unwrap();
            for response in responses.drain(..) {
                pending.push_back(response);
            }
        }

        let byte_count = queued.iter().map(Vec::len).sum::<usize>();
        let write_started = Instant::now();
        if let Err(error) = port.write_queued_wire(&queued, response_expected) {
            let write_elapsed = write_started.elapsed();
            if COMMAND_TRACE.load(Ordering::Acquire) {
                tracing::trace!(
                    records = queued.len(), bytes = byte_count,
                    write_us = write_elapsed.as_micros(),
                    write_ms = write_elapsed.as_secs_f64() * 1_000.0,
                    error = %error,
                "makxd wire write failed"
                );
            }
            let message = error.to_string();
            for tx in completions {
                let _ = tx.send(Err(message.clone()));
            }
            signal.signal_disconnect();
            return;
        }
        let write_elapsed = write_started.elapsed();
        if COMMAND_TRACE.load(Ordering::Acquire) {
            tracing::trace!(
                records = queued.len(),
                bytes = byte_count,
                write_us = write_elapsed.as_micros(),
                write_ms = write_elapsed.as_secs_f64() * 1_000.0,
                "makxd wire write completed"
            );
        }

        for tx in completions {
            let _ = tx.send(Ok(()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;

    struct DatagramCapturePort {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl WirePort for DatagramCapturePort {
        fn try_clone_wire(&self) -> Result<Box<dyn WirePort>> {
            Ok(Box::new(Self {
                writes: Arc::clone(&self.writes),
            }))
        }

        fn write_coalescing_supported(&self) -> bool {
            false
        }

        fn read_wire(&mut self, _bytes: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }

        fn write_all_wire(
            &mut self,
            bytes: &[u8],
            _response_expected: bool,
        ) -> std::io::Result<()> {
            self.writes.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }

        fn write_queued_wire(
            &mut self,
            records: &[Vec<u8>],
            response_expected: bool,
        ) -> std::io::Result<()> {
            for record in records {
                self.write_all_wire(record, response_expected)?;
            }
            Ok(())
        }

        fn drain_wire(&mut self, _timeout: Duration) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn datagram_commands_are_not_coalesced() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel::unbounded();
        let pending = Arc::new(Mutex::new(VecDeque::new()));
        for data in [vec![1, 2], vec![3, 4]] {
            tx.send(WriteRequest::Command {
                data,
                response_tx: None,
                response_id: None,
                write_completion: None,
                expected_nonce: None,
                expected_opcode: None,
            })
            .unwrap();
        }
        drop(tx);
        writer_thread(
            Box::new(DatagramCapturePort {
                writes: Arc::clone(&writes),
            }),
            rx,
            pending,
            Arc::new(ReaderSignal::new()),
        );
        assert_eq!(*writes.lock().unwrap(), [vec![1, 2], vec![3, 4]]);
    }

    struct DrainCapturePort {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl WirePort for DrainCapturePort {
        fn try_clone_wire(&self) -> Result<Box<dyn WirePort>> {
            Ok(Box::new(Self {
                events: Arc::clone(&self.events),
            }))
        }
        fn write_coalescing_supported(&self) -> bool {
            true
        }
        fn read_wire(&mut self, _bytes: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
        fn write_all_wire(
            &mut self,
            _bytes: &[u8],
            _response_expected: bool,
        ) -> std::io::Result<()> {
            self.events.lock().unwrap().push("write");
            Ok(())
        }
        fn drain_wire(&mut self, _timeout: Duration) -> std::io::Result<()> {
            self.events.lock().unwrap().push("drain");
            Ok(())
        }
    }

    fn command(
        data: Vec<u8>,
        completion: Option<channel::Sender<std::result::Result<(), String>>>,
    ) -> WriteRequest {
        WriteRequest::Command {
            data,
            response_tx: None,
            response_id: None,
            write_completion: completion,
            expected_nonce: None,
            expected_opcode: None,
        }
    }

    #[test]
    fn set_completion_fires_after_write_without_drain() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel::unbounded();
        let (completion_tx, completion_rx) = channel::bounded(1);
        tx.send(command(vec![1], Some(completion_tx))).unwrap();
        drop(tx);
        writer_thread(
            Box::new(DrainCapturePort {
                events: Arc::clone(&events),
            }),
            rx,
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(ReaderSignal::new()),
        );
        assert_eq!(completion_rx.recv().unwrap(), Ok(()));
        assert_eq!(*events.lock().unwrap(), ["write"]);
    }

    #[test]
    fn explicit_drain_is_ordered_after_preceding_commands() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel::unbounded();
        let (completion, done) = channel::bounded(1);
        tx.send(command(vec![1], None)).unwrap();
        tx.send(WriteRequest::Drain {
            completion,
            timeout: Duration::from_millis(1),
        })
        .unwrap();
        drop(tx);
        writer_thread(
            Box::new(DrainCapturePort {
                events: Arc::clone(&events),
            }),
            rx,
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(ReaderSignal::new()),
        );
        assert_eq!(done.recv().unwrap(), Ok(()));
        assert_eq!(*events.lock().unwrap(), ["write", "drain"]);
    }
}
