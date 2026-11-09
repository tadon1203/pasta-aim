use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::{
    Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_1},
    Direct3D11::{
        D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BOX, D3D11_CREATE_DEVICE_FLAG, D3D11_QUERY,
        D3D11_QUERY_DATA_TIMESTAMP_DISJOINT, D3D11_QUERY_DESC, D3D11_QUERY_TIMESTAMP,
        D3D11_QUERY_TIMESTAMP_DISJOINT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11CreateDevice,
        ID3D11Device, ID3D11Device4, ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Query,
        ID3D11Resource, ID3D11Texture2D,
    },
    Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
    Dxgi::{
        DXGI_ADAPTER_DESC3, DXGI_ADAPTER_FLAG3_SUPPORT_MONITORED_FENCES,
        DXGI_ADAPTER_FLAG3_SUPPORT_NON_MONITORED_FENCES, DXGI_ERROR_ACCESS_LOST,
        DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIAdapter4,
        IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
    },
};
use windows::core::Interface;

use super::gpu::{CaptureFrame, select_output};
use crate::{
    clock::Clock,
    pipeline::{DisplayInfo, GpuFaultStage},
};

const GPU_QUERY_RING_SIZE: usize = 16;

pub(crate) enum CapturePoll {
    Fresh(AcquiredDesktopFrame),
    /// A timeout, a pointer-only update, or an access loss. The engine counts them.
    NoDesktopUpdate,
}

#[derive(Clone)]
pub(crate) struct AcquiredDesktopFrame {
    texture: ID3D11Texture2D,
    source_box: D3D11_BOX,
    desktop_size: (usize, usize),
    present_qpc: i64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CopyTimes {
    pub copy_submitted_qpc: i64,
    pub ready_qpc: i64,
}

impl AcquiredDesktopFrame {
    pub(crate) fn desktop_size(&self) -> (usize, usize) {
        self.desktop_size
    }

    pub(crate) fn present_qpc(&self) -> i64 {
        self.present_qpc
    }
}

struct CopyTarget {
    resource: ID3D11Resource,
    ready_fence: windows::Win32::Graphics::Direct3D11::ID3D11Fence,
    input_size: (usize, usize),
}

#[derive(Clone, Copy)]
struct FrameLayout {
    desktop_size: (usize, usize),
    source_box: D3D11_BOX,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GpuCopySample {
    pub gpu_copy_ms: f64,
}

struct GpuQuerySlot {
    disjoint: ID3D11Query,
    before: ID3D11Query,
    after: ID3D11Query,
    in_flight: bool,
}

struct D3d11GpuProfiler {
    slots: Vec<GpuQuerySlot>,
    next_slot: usize,
}

impl D3d11GpuProfiler {
    fn new(device: &ID3D11Device4) -> Result<Self> {
        let mut slots = Vec::with_capacity(GPU_QUERY_RING_SIZE);
        for _ in 0..GPU_QUERY_RING_SIZE {
            slots.push(GpuQuerySlot {
                disjoint: create_query(device, D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                before: create_query(device, D3D11_QUERY_TIMESTAMP)?,
                after: create_query(device, D3D11_QUERY_TIMESTAMP)?,
                in_flight: false,
            });
        }
        Ok(Self {
            slots,
            next_slot: 0,
        })
    }

    fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.in_flight = false;
        }
        self.next_slot = 0;
    }

    fn begin(&mut self, context: &ID3D11DeviceContext4) -> Option<usize> {
        let slot_count = self.slots.len();
        for offset in 0..slot_count {
            let index = (self.next_slot + offset) % slot_count;
            if !self.slots[index].in_flight {
                let slot = &mut self.slots[index];
                slot.in_flight = true;
                self.next_slot = (index + 1) % slot_count;
                // SAFETY: queries belong to this immediate context's device.
                unsafe {
                    context.Begin(&slot.disjoint);
                    context.End(&slot.before);
                }
                return Some(index);
            }
        }
        None
    }

    fn end(&mut self, context: &ID3D11DeviceContext4, index: usize) {
        let slot = &self.slots[index];
        // SAFETY: the query sequence is disjoint -> before -> copy -> after -> disjoint.
        unsafe {
            context.End(&slot.after);
            context.End(&slot.disjoint);
        }
    }

    fn try_take(&mut self, context: &ID3D11DeviceContext4) -> Option<GpuCopySample> {
        for slot in &mut self.slots {
            if !slot.in_flight {
                continue;
            }
            // The disjoint query ends last, so the timestamps are finished when it is. A finished
            // disjoint query always has a frequency, so zero means the GPU is not done: keep the slot.
            let Some(disjoint) =
                query_data::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>(context, &slot.disjoint)
            else {
                continue;
            };
            if disjoint.Frequency == 0 {
                continue;
            }
            let (Some(before), Some(after)) = (
                query_data::<u64>(context, &slot.before),
                query_data::<u64>(context, &slot.after),
            ) else {
                continue;
            };
            slot.in_flight = false;
            if disjoint.Disjoint.as_bool() || after < before {
                continue;
            }
            return Some(GpuCopySample {
                gpu_copy_ms: Clock::with_frequency(disjoint.Frequency)
                    .ms(before.cast_signed(), after.cast_signed()),
            });
        }
        None
    }
}

/// `GetData` reports success while the GPU is not done and leaves `data` at its default.
fn query_data<T: Default>(context: &ID3D11DeviceContext4, query: &ID3D11Query) -> Option<T> {
    let mut data = T::default();
    // SAFETY: `data` is writable storage of `size_of::<T>()` bytes, the size of this query's result.
    unsafe {
        context.GetData(
            query,
            Some(std::ptr::from_mut::<T>(&mut data).cast()),
            std::mem::size_of::<T>() as u32,
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
        )
    }
    .ok()?;
    Some(data)
}

fn create_query(device: &ID3D11Device4, query: D3D11_QUERY) -> Result<ID3D11Query> {
    let desc = D3D11_QUERY_DESC {
        Query: query,
        MiscFlags: 0,
    };
    let mut result = None;
    // SAFETY: descriptor and output pointer are valid for CreateQuery.
    unsafe { device.CreateQuery(&desc, Some(&mut result)) }
        .context("failed to create D3D11 query")?;
    result.context("D3D11 returned no query")
}

impl std::fmt::Debug for DxgiCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DxgiCapture").finish_non_exhaustive()
    }
}

pub struct DxgiCapture {
    output: IDXGIOutput1,
    duplication: IDXGIOutputDuplication,
    frame_held: bool,
    device: ID3D11Device4,
    context: ID3D11DeviceContext4,
    info: DisplayInfo,
    gpu_profiler: D3d11GpuProfiler,
    copy_target: Option<CopyTarget>,
    layout: Option<FrameLayout>,
    first_signal_logged: bool,
    last_fault_stage: GpuFaultStage,
}

impl DxgiCapture {
    pub(crate) fn new(adapter_index: u32, output_index: u32) -> Result<Self> {
        tracing::info!(adapter_index, output_index, "initializing DXGI capture");
        let selected = select_output(adapter_index, output_index)?;
        let (device, context) = create_device(&selected.adapter)?;
        let adapter4: IDXGIAdapter4 = selected.adapter.cast()?;
        let mut adapter_desc = DXGI_ADAPTER_DESC3::default();
        unsafe { adapter4.GetDesc3(&mut adapter_desc) }
            .context("failed to query capture adapter fence capabilities")?;
        let device5 = device.cast::<windows::Win32::Graphics::Direct3D11::ID3D11Device5>();
        tracing::info!(
            luid_low = adapter_desc.AdapterLuid.LowPart,
            luid_high = adapter_desc.AdapterLuid.HighPart,
            feature_level = ?unsafe { device.GetFeatureLevel() },
            device5 = device5.is_ok(),
            context4 = true,
            adapter_flags3 = adapter_desc.Flags.0,
            monitored_fence = (adapter_desc.Flags & DXGI_ADAPTER_FLAG3_SUPPORT_MONITORED_FENCES).0 != 0,
            non_monitored_fence = (adapter_desc.Flags & DXGI_ADAPTER_FLAG3_SUPPORT_NON_MONITORED_FENCES).0 != 0,
            "capture_device_info"
        );
        let duplication = create_duplication(&selected.output, &device)?;
        let info = selected.info;
        let gpu_profiler = D3d11GpuProfiler::new(&device)?;
        tracing::info!(adapter_index, output_index, width = info.width, height = info.height, display = %info.display_name, "DXGI capture initialized");
        Ok(Self {
            output: selected.output,
            duplication,
            frame_held: false,
            device,
            context,
            info,
            gpu_profiler,
            copy_target: None,
            layout: None,
            first_signal_logged: false,
            last_fault_stage: GpuFaultStage::CaptureCopy,
        })
    }

    pub fn info(&self) -> &DisplayInfo {
        &self.info
    }

    pub(crate) fn transport_device(&self) -> ID3D11Device4 {
        self.device.clone()
    }

    pub(crate) fn device_removed_reason(&self) -> Option<i32> {
        super::gpu::d3d11_removed_reason(&self.device)
    }

    pub(crate) fn last_fault_stage(&self) -> GpuFaultStage {
        self.last_fault_stage
    }

    pub(crate) fn bind_copy_target(
        &mut self,
        target: &CaptureFrame,
        input_size: (usize, usize),
    ) -> Result<()> {
        if input_size.0 == 0 || input_size.1 == 0 {
            bail!("model input size must be non-zero");
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc points to writable storage for the texture descriptor.
        unsafe { target.texture.GetDesc(&mut desc) };
        if desc.Width as usize != input_size.0 || desc.Height as usize != input_size.1 {
            bail!("shared frame texture dimensions do not match model input");
        }
        let resource = target
            .texture
            .cast::<ID3D11Resource>()
            .context("failed to cast shared destination texture")?;
        self.copy_target = Some(CopyTarget {
            resource,
            ready_fence: target.capture_ready_fence.clone(),
            input_size,
        });
        self.layout = None;
        Ok(())
    }

    pub(crate) fn acquire(&mut self, timeout_ms: u32) -> Result<CapturePoll> {
        if self.frame_held {
            // SAFETY: this instance tracks that the previous successful acquire is held.
            let release_result = unsafe { self.duplication.ReleaseFrame() };
            self.frame_held = false;
            if let Err(error) = release_result {
                if error.code() == DXGI_ERROR_ACCESS_LOST {
                    self.recreate_duplication()?;
                    return Ok(CapturePoll::NoDesktopUpdate);
                }
                return Err(error).context("failed to release previous DXGI frame");
            }
        }

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: output parameters point to writable storage and timeout is caller supplied.
        match unsafe {
            self.duplication
                .AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource)
        } {
            Ok(()) => self.frame_held = true,
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                return Ok(CapturePoll::NoDesktopUpdate);
            }
            Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => {
                self.recreate_duplication()?;
                return Ok(CapturePoll::NoDesktopUpdate);
            }
            Err(error) => return Err(error).context("failed to acquire DXGI desktop frame"),
        }
        if frame_info.LastPresentTime == 0 {
            return Ok(CapturePoll::NoDesktopUpdate);
        }
        let resource = resource.context("DXGI returned no desktop resource")?;
        let texture = resource
            .cast::<ID3D11Texture2D>()
            .context("DXGI desktop resource is not a D3D11 texture")?;
        let layout = if let Some(layout) = self.layout {
            layout
        } else {
            let layout = self.validate_frame(&texture)?;
            self.layout = Some(layout);
            layout
        };
        Ok(CapturePoll::Fresh(AcquiredDesktopFrame {
            texture,
            source_box: layout.source_box,
            desktop_size: layout.desktop_size,
            present_qpc: frame_info.LastPresentTime,
        }))
    }

    fn validate_frame(&self, texture: &ID3D11Texture2D) -> Result<FrameLayout> {
        let target = self
            .copy_target
            .as_ref()
            .context("no copy target is bound for the captured image")?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc points to writable storage for the texture descriptor.
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            bail!("unsupported DXGI pixel format; disable HDR/10-bit output for this build");
        }
        let desktop_size = (desc.Width as usize, desc.Height as usize);
        let (width, height) = target.input_size;
        if width > desktop_size.0 || height > desktop_size.1 {
            bail!(
                "model input {}x{} is larger than captured desktop {}x{}",
                width,
                height,
                desktop_size.0,
                desktop_size.1
            );
        }
        let (left, top) = crate::pipeline::center_crop_origin(desktop_size, (width, height));
        let (left, top) = (left as u32, top as u32);
        let source_box = D3D11_BOX {
            left,
            top,
            front: 0,
            right: left + width as u32,
            bottom: top + height as u32,
            back: 1,
        };
        Ok(FrameLayout {
            desktop_size,
            source_box,
        })
    }

    pub(crate) fn copy_to_target(
        &mut self,
        frame: &AcquiredDesktopFrame,
        ready_value: u64,
        measure: bool,
    ) -> Result<CopyTimes> {
        let target = self
            .copy_target
            .as_ref()
            .context("no copy target is bound")?;
        self.last_fault_stage = GpuFaultStage::CaptureCopy;
        let query_slot = if measure {
            self.gpu_profiler.begin(&self.context)
        } else {
            None
        };
        // SAFETY: both resources share this device, and the source box lies inside the desktop image.
        unsafe {
            self.context.CopySubresourceRegion(
                &target.resource,
                0,
                0,
                0,
                0,
                &*frame.texture,
                0,
                Some(std::ptr::from_ref::<D3D11_BOX>(&frame.source_box)),
            );
        }
        let copy_submitted_qpc = Clock::now();
        if let Some(index) = query_slot {
            self.gpu_profiler.end(&self.context, index);
        }
        self.last_fault_stage = GpuFaultStage::CaptureReadySignal;
        if !self.first_signal_logged {
            tracing::info!(value = ready_value, "capture_fence_first_signal");
        }
        if let Err(error) = unsafe { self.context.Signal(&target.ready_fence, ready_value) } {
            let hr = format!("0x{:08X}", error.code().0.cast_unsigned());
            tracing::error!(value = ready_value, hr, "capture_ready_signal_failed");
            return Err(error).context("failed to signal capture-ready fence");
        }
        // The immediate context buffers commands; without a flush the copy and
        // Signal may not reach the GPU while the D3D12 queue already waits on it.
        unsafe { self.context.Flush() };
        if !self.first_signal_logged {
            tracing::info!(value = ready_value, "capture_fence_first_signal_ok");
            self.first_signal_logged = true;
        }
        Ok(CopyTimes {
            copy_submitted_qpc,
            ready_qpc: Clock::now(),
        })
    }

    pub(crate) fn try_take_gpu_copy_sample(&mut self) -> Option<GpuCopySample> {
        self.gpu_profiler.try_take(&self.context)
    }

    pub(crate) fn reset_gpu_profiler(&mut self) {
        self.gpu_profiler.reset();
    }

    fn recreate_duplication(&mut self) -> Result<()> {
        self.duplication = create_duplication(&self.output, &self.device)?;
        self.frame_held = false;
        self.layout = None;
        tracing::debug!("DXGI desktop duplication recreated after access loss");
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn write_test_image(
        &self,
        target: &CaptureFrame,
        bgra: &[u8],
        (width, height): (usize, usize),
        ready_value: u64,
    ) -> Result<()> {
        anyhow::ensure!(
            bgra.len() == width * height * 4,
            "test image does not match its size"
        );
        let resource = target.texture.cast::<ID3D11Resource>()?;
        // SAFETY: the source holds one BGRA row of `width` pixels for each of `height` rows.
        unsafe {
            self.context.UpdateSubresource(
                &resource,
                0,
                None,
                bgra.as_ptr().cast(),
                (width * 4) as u32,
                0,
            );
        };
        unsafe {
            self.context
                .Signal(&target.capture_ready_fence, ready_value)
        }
        .context("failed to signal the test image")?;
        unsafe { self.context.Flush() };
        Ok(())
    }
}

impl Drop for DxgiCapture {
    fn drop(&mut self) {
        if self.frame_held {
            // SAFETY: this instance still owns one successful, unreleased frame.
            let _ = unsafe { self.duplication.ReleaseFrame() };
            self.frame_held = false;
        }
    }
}

fn create_device(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device4, ID3D11DeviceContext4)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: output pointers are valid and the selected adapter is retained by DXGI.
    let result = unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            None::<&windows::Win32::Foundation::HMODULE>,
            D3D11_CREATE_DEVICE_FLAG::default(),
            Some(&[D3D_FEATURE_LEVEL_11_1]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    };
    if let Err(error) = result {
        return Err(error).context("failed to create D3D11 device");
    }
    let device: ID3D11Device4 = device.context("D3D11 returned no device")?.cast()?;
    let context = context
        .context("D3D11 returned no immediate context")?
        .cast()?;
    Ok((device, context))
}

fn create_duplication(
    output: &IDXGIOutput1,
    device: &ID3D11Device4,
) -> Result<IDXGIOutputDuplication> {
    let device: ID3D11Device = device.cast()?;
    // SAFETY: output and device belong to the same selected adapter.
    unsafe { output.DuplicateOutput(&device) }
        .context("failed to create DXGI Desktop Duplication session")
}
