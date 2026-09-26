use std::fmt::Debug;

use anyhow::{Context, Result};
use ort::{
    AsPointer,
    memory::Allocator,
    session::builder::SessionBuilder,
    value::{PrimitiveTensorElementType, Tensor},
};
use windows::Win32::AI::MachineLearning::DirectML::IDMLDevice;
use windows::{Win32::Graphics::Direct3D12::ID3D12CommandQueue, core::Interface};

/// # Safety
/// `dml_device` and `queue` must belong to the same D3D12 device and outlive the resulting session.
pub(crate) unsafe fn append_dml1(
    builder: &mut SessionBuilder,
    dml_device: &IDMLDevice,
    queue: &ID3D12CommandQueue,
    prepare_id: &str,
) -> Result<()> {
    let api = ort::ep::directml::api().context("ONNX Runtime DirectML API is unavailable")?;
    // SAFETY: caller guarantees that these live COM interfaces are a matching DML device/queue pair.
    let status = unsafe {
        (api.SessionOptionsAppendExecutionProvider_DML1)(
            builder.ptr_mut().cast(),
            dml_device.as_raw(),
            queue.as_raw(),
        )
    };
    tracing::debug!(prepare_id, dml_device = ?dml_device.as_raw(), queue = ?queue.as_raw(), "dml1_registration_pointers");
    // SAFETY: status is returned by ONNX Runtime's DML1 API.
    unsafe { ort::Error::result_from_status(status) }.context("failed to append DirectML DML1")?;
    Ok(())
}

/// # Safety
/// `tensor` must be a live tensor allocated by this DirectML session's allocator.
pub(crate) unsafe fn d3d12_resource_from_dml_tensor<T: PrimitiveTensorElementType + Debug>(
    allocator: &Allocator,
    tensor: &mut Tensor<T>,
    prepare_id: &str,
) -> Result<windows::Win32::Graphics::Direct3D12::ID3D12Resource> {
    let api = ort::ep::directml::api().context("ONNX Runtime DirectML API is unavailable")?;
    let mut raw = std::ptr::null_mut();
    tracing::debug!(prepare_id, allocator = ?allocator.ptr(), tensor_data = ?tensor.data_ptr_mut(), "dml_tensor_resource_pointers");
    // SAFETY: caller guarantees the DML tensor allocation and allocator are from the same session.
    let status = unsafe {
        (api.GetD3D12ResourceFromAllocation)(
            allocator.ptr().cast_mut(),
            tensor.data_ptr_mut().cast(),
            &mut raw,
        )
    };
    // SAFETY: status is returned by ONNX Runtime's DirectML API.
    unsafe { ort::Error::result_from_status(status) }
        .context("failed to get D3D12 resource for DirectML tensor")?;
    if raw.is_null() {
        anyhow::bail!("DirectML returned a null D3D12 tensor resource");
    }
    // SAFETY: GetD3D12ResourceFromAllocation returns an AddRef'd ID3D12Resource per ORT contract.
    Ok(unsafe { windows::Win32::Graphics::Direct3D12::ID3D12Resource::from_raw(raw.cast()) })
}
