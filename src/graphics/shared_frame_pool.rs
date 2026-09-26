use anyhow::{Context, Result, bail};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Graphics::{
            Direct3D11::{
                D3D11_BIND_SHADER_RESOURCE, D3D11_FENCE_FLAG_SHARED, D3D11_RESOURCE_MISC_SHARED,
                D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
                ID3D11Device4, ID3D11Device5, ID3D11Fence, ID3D11Texture2D,
            },
            Direct3D12::{ID3D12Fence, ID3D12Resource},
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIResource1,
            },
        },
    },
    core::{Interface, PCWSTR},
};

use super::{
    device::GpuDeviceContext,
    frame_mailbox::{FramePublisher, GPU_FRAME_SLOTS},
    gpu_diagnostics::{OWN_OBJECT_PREFIX, name_d3d11, name_d3d12},
};

const GENERIC_ALL: u32 = 0x1000_0000;

#[derive(Clone)]
pub struct CaptureFramePool {
    pub textures: [ID3D11Texture2D; GPU_FRAME_SLOTS],
    pub capture_ready_fence: ID3D11Fence,
    pub reuse_fence: ID3D11Fence,
    pub mailbox: FramePublisher,
}

pub struct InferenceFramePool {
    pub resources: [ID3D12Resource; GPU_FRAME_SLOTS],
    pub capture_ready_fence: ID3D12Fence,
    pub reuse_fence: ID3D12Fence,
    pub mailbox: FramePublisher,
}

impl CaptureFramePool {
    pub fn completed_reuse_value(&self) -> Result<u64> {
        let completed = unsafe { self.reuse_fence.GetCompletedValue() };
        if completed == u64::MAX {
            bail!("D3D11 reuse fence reports device removed");
        }
        Ok(completed)
    }

    pub fn reclaim_completed(&self) -> Result<()> {
        self.mailbox.reclaim(self.completed_reuse_value()?)
    }
}

/// Creates the shared frame transport with the **D3D11 capture device as owner**.
///
/// Textures and fences are created on the D3D11 device (`SHARED | SHARED_NTHANDLE`,
/// `D3D11_FENCE_FLAG_SHARED`) and only *opened* on the D3D12 device through
/// `ID3D12Device::OpenSharedHandle`. The producer keeps the native resource and the
/// consumer imports it, so the D3D11 runtime never has to accept a D3D12-authored
/// resource description (the previous direction failed with E_INVALIDARG).
pub fn create_shared_frame_pool(
    gpu: &GpuDeviceContext,
    capture_device: &ID3D11Device4,
    mailbox: FramePublisher,
    width: usize,
    height: usize,
) -> Result<(CaptureFramePool, InferenceFramePool)> {
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

    let mut textures = Vec::with_capacity(GPU_FRAME_SLOTS);
    let mut resources = Vec::with_capacity(GPU_FRAME_SLOTS);
    for slot in 0..GPU_FRAME_SLOTS {
        let mut texture = None;
        unsafe { capture_device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .with_context(|| format!("failed to create D3D11 shared frame texture slot={slot}"))?;
        let texture: ID3D11Texture2D = texture.context("D3D11 returned no shared texture")?;
        name_d3d11(&texture, &format!("{OWN_OBJECT_PREFIX}capture-slot-{slot}"));

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
            .with_context(|| format!("failed to create shared texture handle slot={slot}"))?,
        );
        let mut opened: Option<ID3D12Resource> = None;
        let open_result = unsafe { gpu.device().OpenSharedHandle(handle.0, &mut opened) };
        let resource = match open_result {
            Ok(()) => opened.context("D3D12 returned no opened shared texture")?,
            Err(error) => {
                super::gpu_diagnostics::log_d3d12_messages(gpu.device(), true);
                super::gpu_diagnostics::log_dred_report(gpu.device());
                tracing::error!(
                    slot,
                    hresult = format_args!("0x{:08X}", error.code().0 as u32),
                    width = width_u32,
                    height = height_u32,
                    format = "B8G8R8A8_UNORM",
                    misc_flags = desc.MiscFlags,
                    adapter_luid_low = gpu.adapter_luid().low,
                    adapter_luid_high = gpu.adapter_luid().high,
                    "D3D12 OpenSharedHandle failed for D3D11-owned frame texture"
                );
                return Err(error)
                    .with_context(|| format!("D3D12 failed to open shared texture slot={slot}"));
            }
        };
        name_d3d12(&resource, &format!("{OWN_OBJECT_PREFIX}inference-slot-{slot}"));
        let opened = unsafe { resource.GetDesc() };
        if opened.Width != u64::from(width_u32)
            || opened.Height != height_u32
            || opened.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || opened.SampleDesc.Count != 1
        {
            bail!(
                "opened shared texture descriptor mismatch: slot={slot}, expected={width_u32}x{height_u32} B8G8R8A8_UNORM samples=1, actual={}x{} format={:?} samples={}",
                opened.Width,
                opened.Height,
                opened.Format,
                opened.SampleDesc.Count
            );
        }
        drop(handle);
        textures.push(texture);
        resources.push(resource);
    }

    let (capture_ready_initial, reuse_initial) = mailbox.shared_fence_initial_values();
    let (capture_ready_d3d12, capture_ready_fence) =
        create_shared_fence(gpu, &device5, "capture-ready", capture_ready_initial)?;
    let (reuse_d3d12, reuse_fence) = create_shared_fence(gpu, &device5, "reuse", reuse_initial)?;
    let textures: [ID3D11Texture2D; GPU_FRAME_SLOTS] = textures
        .try_into()
        .map_err(|_| anyhow::anyhow!("failed to create all shared D3D11 textures"))?;
    let resources: [ID3D12Resource; GPU_FRAME_SLOTS] = resources
        .try_into()
        .map_err(|_| anyhow::anyhow!("failed to create all shared D3D12 resources"))?;
    Ok((
        CaptureFramePool {
            textures,
            capture_ready_fence,
            reuse_fence,
            mailbox: mailbox.clone(),
        },
        InferenceFramePool {
            resources,
            capture_ready_fence: capture_ready_d3d12,
            reuse_fence: reuse_d3d12,
            mailbox,
        },
    ))
}

/// The D3D11 device creates the fence and D3D12 opens it, so both sides observe a
/// single monotonically increasing timeline and the hot path never waits on the CPU.
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
    name_d3d12(&d3d12_fence, &format!("{OWN_OBJECT_PREFIX}{name}-fence"));
    Ok((d3d12_fence, d3d11_fence))
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe { CloseHandle(self.0) }.ok();
        }
    }
}
