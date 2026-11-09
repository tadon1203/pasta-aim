use std::mem::{ManuallyDrop, size_of};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::{BOOL, CloseHandle, HANDLE, WAIT_OBJECT_0},
        Graphics::{
            Direct3D::D3D_FEATURE_LEVEL_11_0,
            Direct3D11::{
                D3D11_BIND_SHADER_RESOURCE, D3D11_FENCE_FLAG_SHARED, D3D11_RESOURCE_MISC_SHARED,
                D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
                ID3D11Device4, ID3D11Device5, ID3D11Fence, ID3D11Texture2D,
            },
            Direct3D12::{
                D3D12_COMMAND_LIST_TYPE_COMPUTE, D3D12_COMMAND_QUEUE_DESC,
                D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12_COMMAND_QUEUE_PRIORITY,
                D3D12_COMMAND_QUEUE_PRIORITY_HIGH, D3D12_COMMAND_QUEUE_PRIORITY_NORMAL,
                D3D12_FENCE_FLAG_NONE, D3D12_MESSAGE, D3D12_RESOURCE_BARRIER,
                D3D12_RESOURCE_BARRIER_0, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                D3D12_RESOURCE_STATES, D3D12_RESOURCE_TRANSITION_BARRIER, D3D12CreateDevice,
                D3D12GetDebugInterface, ID3D12CommandQueue, ID3D12Debug, ID3D12Device, ID3D12Fence,
                ID3D12InfoQueue, ID3D12Resource,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                CreateDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ERROR_NOT_FOUND, DXGI_OUTPUT_DESC,
                DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter1,
                IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIResource1,
            },
            Gdi::{
                DEVMODEW, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS,
                EnumDisplaySettingsExW,
            },
        },
        System::Threading::{CreateEventW, INFINITE, WaitForSingleObject},
    },
    core::{Interface, PCWSTR},
};

use crate::pipeline::{AdapterLuid, DisplayInfo};

const GENERIC_ALL: u32 = 0x1000_0000;

pub(super) struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe { CloseHandle(self.0) }.ok();
        }
    }
}

pub(super) fn transition(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

pub(super) struct QueueFence {
    fence: ID3D12Fence,
    event: OwnedHandle,
    next_value: u64,
    label: &'static str,
}

impl QueueFence {
    pub(super) fn new(device: &ID3D12Device, label: &'static str) -> Result<Self> {
        let fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }
            .with_context(|| format!("failed to create the {label} fence"))?;
        let event = OwnedHandle(
            unsafe { CreateEventW(None, BOOL(0), BOOL(0), PCWSTR::null()) }
                .with_context(|| format!("failed to create the {label} event"))?,
        );
        Ok(Self {
            fence,
            event,
            next_value: 1,
            label,
        })
    }

    pub(super) fn signal_and_wait(&mut self, queue: &ID3D12CommandQueue, spin: bool) -> Result<()> {
        let label = self.label;
        let value = self.next_value;
        self.next_value = value.saturating_add(1);
        unsafe { queue.Signal(&self.fence, value) }
            .with_context(|| format!("failed to signal the {label} fence"))?;
        // A removed device reports `u64::MAX`, which ends the wait.
        if spin {
            while unsafe { self.fence.GetCompletedValue() } < value {
                std::hint::spin_loop();
            }
        } else if unsafe { self.fence.GetCompletedValue() } < value {
            unsafe { self.fence.SetEventOnCompletion(value, self.event.0) }
                .with_context(|| format!("failed to arm the {label} fence event"))?;
            let result = unsafe { WaitForSingleObject(self.event.0, INFINITE) };
            if result != WAIT_OBJECT_0 {
                bail!("{label} fence wait failed: {result:?}");
            }
        }
        if fence_reports_removal(unsafe { self.fence.GetCompletedValue() }) {
            bail!("D3D12 {label} fence reports device removed");
        }
        Ok(())
    }
}

pub(crate) fn hex(value: Option<i32>) -> String {
    value.map_or_else(
        || "0x00000000".to_owned(),
        |value| format!("0x{:08X}", value.cast_unsigned()),
    )
}

fn factory() -> Result<IDXGIFactory1> {
    // SAFETY: CreateDXGIFactory1 initializes and returns the requested COM interface.
    unsafe { CreateDXGIFactory1() }.context("failed to create DXGI factory")
}

fn adapters(factory: &IDXGIFactory1) -> impl Iterator<Item = Result<(u32, IDXGIAdapter1)>> + '_ {
    (0..).map_while(move |index| match unsafe { factory.EnumAdapters1(index) } {
        Ok(adapter) => Some(Ok((index, adapter))),
        Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => None,
        Err(error) => Some(Err(error).context("failed to enumerate DXGI adapters")),
    })
}

fn outputs(adapter: &IDXGIAdapter1) -> impl Iterator<Item = Result<(u32, IDXGIOutput)>> + '_ {
    (0..).map_while(move |index| match unsafe { adapter.EnumOutputs(index) } {
        Ok(output) => Some(Ok((index, output))),
        Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => None,
        Err(error) => Some(Err(error).context("failed to enumerate DXGI outputs")),
    })
}

pub(super) fn enumerate_displays() -> Result<Vec<DisplayInfo>> {
    let factory = factory()?;
    let mut result = Vec::new();
    for adapter in adapters(&factory) {
        let (adapter_index, adapter) = adapter?;
        for output in outputs(&adapter) {
            let (output_index, output) = output?;
            result.push(current_display_info(
                adapter_index,
                output_index,
                &adapter,
                &output,
            )?);
        }
    }
    Ok(result)
}

pub(super) struct SelectedOutput {
    pub adapter: IDXGIAdapter1,
    pub output: IDXGIOutput1,
    pub info: DisplayInfo,
}

pub(super) fn select_output(adapter_index: u32, output_index: u32) -> Result<SelectedOutput> {
    let factory = factory()?;
    let (_, adapter) = adapters(&factory)
        .nth(adapter_index as usize)
        .ok_or_else(|| anyhow!("no DXGI adapter {adapter_index}"))??;
    let (_, output) = outputs(&adapter)
        .nth(output_index as usize)
        .ok_or_else(|| anyhow!("no DXGI output {output_index} on adapter {adapter_index}"))??;
    let info = current_display_info(adapter_index, output_index, &adapter, &output)?;
    let output = output
        .cast::<IDXGIOutput1>()
        .context("selected DXGI output does not support IDXGIOutput1")?;
    Ok(SelectedOutput {
        adapter,
        output,
        info,
    })
}

fn current_display_info(
    adapter_index: u32,
    output_index: u32,
    adapter: &IDXGIAdapter1,
    output: &IDXGIOutput,
) -> Result<DisplayInfo> {
    let mut adapter_desc = DXGI_ADAPTER_DESC1::default();
    // SAFETY: the descriptor points to writable storage of the documented type.
    unsafe { adapter.GetDesc1(&mut adapter_desc) }.context("failed to query DXGI adapter")?;

    let mut output_desc = DXGI_OUTPUT_DESC::default();
    // SAFETY: the descriptor points to writable storage of the documented type.
    unsafe { output.GetDesc(&mut output_desc) }.context("failed to query DXGI output")?;

    let mut mode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: DeviceName is null-terminated by DXGI and mode is initialized with its size.
    let mode_found = unsafe {
        EnumDisplaySettingsExW(
            PCWSTR(output_desc.DeviceName.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut mode,
            ENUM_DISPLAY_SETTINGS_FLAGS(0),
        )
    };
    if !mode_found.as_bool() {
        return Err(anyhow!(
            "failed to query current display mode for {}",
            wide_string(&output_desc.DeviceName)
        ));
    }

    Ok(DisplayInfo {
        adapter_index,
        output_index,
        adapter_luid: AdapterLuid {
            low: adapter_desc.AdapterLuid.LowPart,
            high: adapter_desc.AdapterLuid.HighPart,
        },
        adapter_name: wide_string(&adapter_desc.Description),
        display_name: wide_string(&output_desc.DeviceName),
        width: mode.dmPelsWidth,
        height: mode.dmPelsHeight,
        refresh_hz: f64::from(mode.dmDisplayFrequency),
    })
}

fn wide_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

pub(super) fn d3d11_removed_reason(device: &ID3D11Device4) -> Option<i32> {
    unsafe { device.GetDeviceRemovedReason() }
        .err()
        .map(|error| error.code().0)
}

pub(super) fn d3d12_removed_reason(device: &ID3D12Device) -> Option<i32> {
    unsafe { device.GetDeviceRemovedReason() }
        .err()
        .map(|error| error.code().0)
}

pub(super) fn fence_reports_removal(completed: u64) -> bool {
    completed == u64::MAX
}

pub(super) fn enable_debug_layer() -> Result<()> {
    let mut debug: Option<ID3D12Debug> = None;
    unsafe { D3D12GetDebugInterface(&mut debug) }.context("failed to get D3D12 debug interface")?;
    let debug: ID3D12Debug = debug.ok_or_else(|| anyhow!("D3D12 debug interface was empty"))?;
    unsafe { debug.EnableDebugLayer() };
    Ok(())
}

fn drain_debug_messages(device: &ID3D12Device) -> Vec<String> {
    let Ok(queue) = device.cast::<ID3D12InfoQueue>() else {
        return Vec::new();
    };
    let count = unsafe { queue.GetNumStoredMessages() };
    let mut messages = Vec::new();
    for index in 0..count {
        let mut length = 0usize;
        if unsafe { queue.GetMessage(index, None, &mut length) }.is_err() {
            continue;
        }
        let mut buffer = vec![0u64; length.div_ceil(size_of::<u64>())];
        let message = buffer.as_mut_ptr().cast::<D3D12_MESSAGE>();
        if unsafe { queue.GetMessage(index, Some(message), &mut length) }.is_err() {
            continue;
        }
        // SAFETY: the buffer is aligned and holds the message that the info queue wrote into it.
        let message = unsafe { &*message };
        let description = if message.pDescription.is_null() || message.DescriptionByteLength == 0 {
            String::new()
        } else {
            // SAFETY: the info queue keeps the description valid for its byte length.
            let bytes = unsafe {
                std::slice::from_raw_parts(message.pDescription, message.DescriptionByteLength)
            };
            String::from_utf8_lossy(bytes)
                .trim_end_matches(' ')
                .to_owned()
        };
        messages.push(format!(
            "severity={:?} category={:?} id={:?} {description}",
            message.Severity, message.Category, message.ID
        ));
    }
    unsafe { queue.ClearStoredMessages() };
    messages
}

pub(super) fn log_fault(
    gpu: &GpuDeviceContext,
    d3d11_removed: Option<i32>,
    pipeline_id: u64,
    stage: &str,
    error: &anyhow::Error,
) {
    let hresult = error
        .downcast_ref::<windows::core::Error>()
        .map(|error| error.code().0);
    let messages = drain_debug_messages(gpu.device());
    tracing::error!(
        pipeline_id,
        stage,
        error = %format!("{error:#}"),
        hresult = %hex(hresult),
        d3d11_removed_reason = %hex(d3d11_removed),
        d3d12_removed_reason = %hex(gpu.removed_reason()),
        debug_messages = %messages.join(" | "),
        "gpu_fault"
    );
}

fn create_compute_queue(device: &ID3D12Device) -> Result<ID3D12CommandQueue> {
    let queue = |priority: D3D12_COMMAND_QUEUE_PRIORITY| {
        let desc = D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_COMPUTE,
            Priority: priority.0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        };
        // SAFETY: `desc` is a valid queue description for this device.
        unsafe { device.CreateCommandQueue::<ID3D12CommandQueue>(&desc) }
    };
    match queue(D3D12_COMMAND_QUEUE_PRIORITY_HIGH) {
        Ok(queue) => {
            tracing::info!("D3D12 compute queue created with the high priority");
            Ok(queue)
        }
        Err(error) => {
            tracing::warn!(%error, "the high queue priority is refused; using the normal priority");
            queue(D3D12_COMMAND_QUEUE_PRIORITY_NORMAL)
                .context("failed to create D3D12 compute queue")
        }
    }
}

pub struct GpuDeviceContext {
    adapter_luid: AdapterLuid,
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    idle: QueueFence,
}

impl std::fmt::Debug for GpuDeviceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuDeviceContext").finish_non_exhaustive()
    }
}

impl GpuDeviceContext {
    pub fn new(adapter_luid: AdapterLuid) -> Result<Self> {
        let factory = factory()?;
        let mut selected = None;
        for adapter in adapters(&factory) {
            let (_, adapter) = adapter?;
            let mut desc = DXGI_ADAPTER_DESC1::default();
            unsafe { adapter.GetDesc1(&mut desc) }.context("failed to read adapter LUID")?;
            if desc.AdapterLuid.LowPart == adapter_luid.low
                && desc.AdapterLuid.HighPart == adapter_luid.high
            {
                selected = Some(adapter);
                break;
            }
        }
        let adapter = selected.ok_or_else(|| {
            anyhow!(
                "no DXGI adapter matches LUID {}:{}",
                adapter_luid.high,
                adapter_luid.low
            )
        })?;
        let mut device = None;
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }
            .context("failed to create D3D12 device")?;
        let device: ID3D12Device = device.context("D3D12CreateDevice returned no device")?;
        let queue = create_compute_queue(&device)?;
        let idle = QueueFence::new(&device, "teardown")?;
        tracing::info!(
            adapter_luid_low = adapter_luid.low,
            adapter_luid_high = adapter_luid.high,
            "D3D12 device and compute queue created"
        );
        Ok(Self {
            adapter_luid,
            device,
            queue,
            idle,
        })
    }

    pub fn adapter_luid(&self) -> AdapterLuid {
        self.adapter_luid
    }

    pub fn device(&self) -> &ID3D12Device {
        &self.device
    }

    pub fn queue(&self) -> &ID3D12CommandQueue {
        &self.queue
    }

    pub fn removed_reason(&self) -> Option<i32> {
        d3d12_removed_reason(&self.device)
    }

    pub fn wait_idle(&mut self) -> Result<()> {
        self.idle.signal_and_wait(&self.queue, false)
    }
}

impl std::fmt::Debug for CaptureFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureFrame").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for InferenceFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceFrame").finish_non_exhaustive()
    }
}

pub struct CaptureFrame {
    pub texture: ID3D11Texture2D,
    pub capture_ready_fence: ID3D11Fence,
}

pub struct InferenceFrame {
    pub resource: ID3D12Resource,
    pub capture_ready_fence: ID3D12Fence,
    pub reuse_fence: ID3D12Fence,
}

impl InferenceFrame {
    pub fn reuse_completed(&self) -> u64 {
        // SAFETY: the fence is a live COM object owned by this frame.
        unsafe { self.reuse_fence.GetCompletedValue() }
    }
}

/// The D3D11 device owns the texture and the fence; D3D12 only opens them.
pub fn create_shared_frame(
    gpu: &GpuDeviceContext,
    capture_device: &ID3D11Device4,
    width: usize,
    height: usize,
) -> Result<(CaptureFrame, InferenceFrame)> {
    if width == 0 || height == 0 {
        bail!("capture frame dimensions must be non-zero");
    }
    let width_u32 = u32::try_from(width).context("shared texture width exceeds D3D limits")?;
    let height_u32 = u32::try_from(height).context("shared texture height exceeds D3D limits")?;
    let device5: ID3D11Device5 = capture_device
        .cast()
        .context("capture D3D11 device does not support shared fences")?;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width_u32,
        Height: height_u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
    };

    let mut texture = None;
    unsafe { capture_device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .context("failed to create D3D11 shared frame texture")?;
    let texture: ID3D11Texture2D = texture.context("D3D11 returned no shared texture")?;

    let dxgi: IDXGIResource1 = texture
        .cast()
        .context("shared texture does not expose IDXGIResource1")?;
    let handle = OwnedHandle(
        unsafe {
            dxgi.CreateSharedHandle(
                None,
                DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE,
                PCWSTR::null(),
            )
        }
        .context("failed to create shared texture handle")?,
    );
    let mut opened: Option<ID3D12Resource> = None;
    let open_result = unsafe { gpu.device().OpenSharedHandle(handle.0, &mut opened) };
    let resource = match open_result {
        Ok(()) => opened.context("D3D12 returned no opened shared texture")?,
        Err(error) => {
            tracing::error!(
                hresult = %hex(Some(error.code().0)),
                width = width_u32,
                height = height_u32,
                format = "B8G8R8A8_UNORM",
                misc_flags = desc.MiscFlags,
                adapter_luid_low = gpu.adapter_luid().low,
                adapter_luid_high = gpu.adapter_luid().high,
                "D3D12 OpenSharedHandle failed for D3D11-owned frame texture"
            );
            return Err(error).context("D3D12 failed to open shared texture");
        }
    };
    let opened = unsafe { resource.GetDesc() };
    if opened.Width != u64::from(width_u32)
        || opened.Height != height_u32
        || opened.Format != DXGI_FORMAT_B8G8R8A8_UNORM
        || opened.SampleDesc.Count != 1
    {
        bail!(
            "opened shared texture descriptor mismatch: single frame, expected={width_u32}x{height_u32} B8G8R8A8_UNORM samples=1, actual={}x{} format={:?} samples={}",
            opened.Width,
            opened.Height,
            opened.Format,
            opened.SampleDesc.Count
        );
    }
    drop(handle);

    let (capture_ready_d3d12, capture_ready_fence) =
        create_shared_fence(gpu, &device5, "capture-ready", 0)?;
    let reuse_d3d12 = unsafe { gpu.device().CreateFence(0, D3D12_FENCE_FLAG_NONE) }
        .context("failed to create preprocess completion fence")?;
    Ok((
        CaptureFrame {
            texture,
            capture_ready_fence,
        },
        InferenceFrame {
            resource,
            capture_ready_fence: capture_ready_d3d12,
            reuse_fence: reuse_d3d12,
        },
    ))
}

fn create_shared_fence(
    gpu: &GpuDeviceContext,
    capture_device: &ID3D11Device5,
    name: &str,
    initial_value: u64,
) -> Result<(ID3D12Fence, ID3D11Fence)> {
    let mut created: Option<ID3D11Fence> = None;
    unsafe { capture_device.CreateFence(initial_value, D3D11_FENCE_FLAG_SHARED, &mut created) }
        .with_context(|| format!("failed to create shared D3D11 {name} fence"))?;
    let d3d11_fence = created.context("D3D11 returned no shared fence")?;
    let handle = OwnedHandle(
        unsafe { d3d11_fence.CreateSharedHandle(None, GENERIC_ALL, PCWSTR::null()) }
            .with_context(|| format!("failed to create shared {name} fence handle"))?,
    );
    let mut opened: Option<ID3D12Fence> = None;
    unsafe { gpu.device().OpenSharedHandle(handle.0, &mut opened) }
        .with_context(|| format!("D3D12 failed to open shared {name} fence"))?;
    let d3d12_fence = opened.context("D3D12 returned no opened shared fence")?;
    Ok((d3d12_fence, d3d11_fence))
}
