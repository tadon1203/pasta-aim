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
            Direct3D12::{D3D12_FENCE_FLAG_NONE, ID3D12Fence, ID3D12Resource},
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
    gpu_diagnostics::{OWN_OBJECT_PREFIX, name_d3d11, name_d3d12},
};

const GENERIC_ALL: u32 = 0x1000_0000;

pub struct CaptureFrame {
    pub texture: ID3D11Texture2D,
    pub capture_ready_fence: ID3D11Fence,
}

pub struct InferenceFrame {
    pub resource: ID3D12Resource,
    pub capture_ready_fence: ID3D12Fence,
    pub reuse_fence: ID3D12Fence,
}

/// Creates the shared frame transport with the **D3D11 capture device as owner**.
///
/// The texture and capture-ready fence are created on the D3D11 device (`SHARED | SHARED_NTHANDLE`,
/// `D3D11_FENCE_FLAG_SHARED`) and only *opened* on the D3D12 device through
/// `ID3D12Device::OpenSharedHandle`. The producer keeps the native resource and the
/// consumer imports it, so the D3D11 runtime never has to accept a D3D12-authored
/// resource description (the previous direction failed with E_INVALIDARG).
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

    let (texture, resource) = {
        let mut texture = None;
        unsafe { capture_device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .context("failed to create D3D11 shared frame texture")?;
        let texture: ID3D11Texture2D = texture.context("D3D11 returned no shared texture")?;
        name_d3d11(&texture, &format!("{OWN_OBJECT_PREFIX}capture-frame"));

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
                super::gpu_diagnostics::log_d3d12_messages(gpu.device(), true);
                super::gpu_diagnostics::log_dred_report(gpu.device());
                tracing::error!(
                    hresult = format_args!("0x{:08X}", error.code().0 as u32),
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
        name_d3d12(&resource, &format!("{OWN_OBJECT_PREFIX}inference-frame"));
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
        (texture, resource)
    };

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
