use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::{BOOL, CloseHandle, HANDLE, WAIT_OBJECT_0},
        Graphics::{
            Direct3D::D3D_FEATURE_LEVEL_11_0,
            Direct3D12::{
                D3D12_COMMAND_LIST_TYPE_COMPUTE, D3D12_COMMAND_QUEUE_DESC,
                D3D12_COMMAND_QUEUE_FLAG_NONE, D3D12_COMMAND_QUEUE_PRIORITY_GLOBAL_REALTIME,
                D3D12_COMMAND_QUEUE_PRIORITY_HIGH, D3D12_COMMAND_QUEUE_PRIORITY_NORMAL,
                D3D12_FENCE_FLAG_NONE, D3D12CreateDevice, ID3D12CommandQueue, ID3D12Device,
                ID3D12Fence,
            },
            Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1},
        },
        System::Threading::{CreateEventW, INFINITE, WaitForSingleObject},
    },
    core::PCWSTR,
};

use super::gpu_diagnostics::{OWN_OBJECT_PREFIX, name_d3d12};
use crate::inference::AdapterLuid;

pub struct GpuDeviceContext {
    adapter_luid: AdapterLuid,
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    idle_fence: ID3D12Fence,
    idle_event: HANDLE,
    next_idle_value: u64,
}

impl GpuDeviceContext {
    pub fn new(adapter_luid: AdapterLuid) -> Result<Self> {
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.context("failed to create DXGI factory for D3D12")?;
        let mut selected = None;
        for index in 0.. {
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(error).context("failed to enumerate D3D12 adapters"),
            };
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
        name_d3d12(&queue, &format!("{OWN_OBJECT_PREFIX}compute-queue"));
        let idle_fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }
            .context("failed to create teardown fence")?;
        let idle_event = unsafe { CreateEventW(None, BOOL(0), BOOL(0), PCWSTR::null()) }
            .context("failed to create teardown event")?;
        tracing::info!(
            adapter_luid_low = adapter_luid.low,
            adapter_luid_high = adapter_luid.high,
            "D3D12 device and compute queue created"
        );
        Ok(Self {
            adapter_luid,
            device,
            queue,
            idle_fence,
            idle_event,
            next_idle_value: 1,
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

    /// Waits for all work submitted to the compute queue. Teardown/rebuild only.
    pub fn wait_idle(&mut self) -> Result<()> {
        let value = self.next_idle_value;
        self.next_idle_value = value.saturating_add(1);
        unsafe { self.queue.Signal(&self.idle_fence, value) }
            .context("failed to signal teardown fence")?;
        let completed = unsafe { self.idle_fence.GetCompletedValue() };
        if completed == u64::MAX {
            bail!("D3D12 teardown fence reports device removed");
        }
        if completed < value {
            unsafe { self.idle_fence.SetEventOnCompletion(value, self.idle_event) }
                .context("failed to arm teardown fence event")?;
            let result = unsafe { WaitForSingleObject(self.idle_event, INFINITE) };
            if result != WAIT_OBJECT_0 {
                bail!("teardown fence wait failed: {result:?}");
            }
            if unsafe { self.idle_fence.GetCompletedValue() } == u64::MAX {
                bail!("D3D12 teardown fence reports device removed after wait");
            }
        }
        Ok(())
    }
}

/// Creates the compute queue at the highest priority the process may use, so
/// small inference jobs are not queued behind the game's frames.
fn create_compute_queue(device: &ID3D12Device) -> Result<ID3D12CommandQueue> {
    let mut last_error = None;
    for (priority, name) in [
        (
            D3D12_COMMAND_QUEUE_PRIORITY_GLOBAL_REALTIME,
            "global realtime",
        ),
        (D3D12_COMMAND_QUEUE_PRIORITY_HIGH, "high"),
        (D3D12_COMMAND_QUEUE_PRIORITY_NORMAL, "normal"),
    ] {
        let desc = D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_COMPUTE,
            Priority: priority.0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        };
        // SAFETY: `desc` is a valid queue description for this device.
        match unsafe { device.CreateCommandQueue::<ID3D12CommandQueue>(&desc) } {
            Ok(queue) => {
                tracing::info!(priority = name, "D3D12 compute queue priority");
                return Ok(queue);
            }
            Err(error) => {
                tracing::debug!(priority = name, %error, "D3D12 queue priority unavailable");
                last_error = Some(error);
            }
        }
    }
    Err(last_error.expect("at least one priority was tried"))
        .context("failed to create D3D12 compute queue")
}

impl Drop for GpuDeviceContext {
    fn drop(&mut self) {
        if !self.idle_event.is_invalid() {
            let _ = unsafe { CloseHandle(self.idle_event) };
        }
    }
}
