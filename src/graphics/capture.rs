use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::{
    Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_1},
    Direct3D11::{
        D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BOX, D3D11_QUERY,
        D3D11_QUERY_DATA_TIMESTAMP_DISJOINT, D3D11_QUERY_DESC, D3D11_QUERY_TIMESTAMP,
        D3D11_QUERY_TIMESTAMP_DISJOINT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11CreateDevice,
        ID3D11Device, ID3D11Device4, ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Fence,
        ID3D11Query, ID3D11Resource, ID3D11Texture2D,
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

use super::gpu_diagnostics::GpuFaultStage;
use super::profile_sampler::ProfileSampler;
use super::{DisplayInfo, select_output};
use crate::timing::QpcClock;

const GPU_QUERY_RING_SIZE: usize = 16;

pub(crate) enum CapturePoll {
    Fresh(AcquiredDesktopFrame),
    NoDesktopUpdate(NoDesktopUpdateReason),
}

#[derive(Clone)]
pub(crate) struct AcquiredDesktopFrame {
    texture: ID3D11Texture2D,
    attempt_start_qpc: i64,
    acquired_qpc: i64,
    desktop_size: (usize, usize),
    present_qpc: i64,
    accumulated_frames: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NoDesktopUpdateReason {
    Timeout,
    PointerOnly,
    AccessLost,
}

/// The stage boundaries of a submitted copy, as QPC ticks.
pub(crate) struct CaptureSubmit {
    pub present_qpc: i64,
    pub attempt_start_qpc: i64,
    pub acquired_qpc: i64,
    pub copy_submitted_qpc: i64,
    pub ready_qpc: i64,
    pub desktop_size: (usize, usize),
}

impl AcquiredDesktopFrame {
    pub(crate) fn accumulated_frames(&self) -> u32 {
        self.accumulated_frames
    }
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
    sampler: ProfileSampler,
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
            sampler: ProfileSampler::default(),
        })
    }

    fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.in_flight = false;
        }
        self.next_slot = 0;
        self.sampler.reset();
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
            let mut disjoint = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            let mut before = 0u64;
            let mut after = 0u64;
            let ready = unsafe {
                context
                    .GetData(
                        &slot.disjoint,
                        Some((&mut disjoint as *mut D3D11_QUERY_DATA_TIMESTAMP_DISJOINT).cast()),
                        std::mem::size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() as u32,
                        D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                    )
                    .is_ok()
                    && context
                        .GetData(
                            &slot.before,
                            Some((&mut before as *mut u64).cast()),
                            std::mem::size_of::<u64>() as u32,
                            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                        )
                        .is_ok()
                    && context
                        .GetData(
                            &slot.after,
                            Some((&mut after as *mut u64).cast()),
                            std::mem::size_of::<u64>() as u32,
                            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                        )
                        .is_ok()
            };
            if !ready {
                continue;
            }
            slot.in_flight = false;
            if disjoint.Disjoint.as_bool() || disjoint.Frequency == 0 || after < before {
                continue;
            }
            return Some(GpuCopySample {
                gpu_copy_ms: (after - before) as f64 * 1000.0 / disjoint.Frequency as f64,
            });
        }
        None
    }
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

pub struct DxgiCapture {
    output: IDXGIOutput1,
    duplication: IDXGIOutputDuplication,
    frame_held: bool,
    device: ID3D11Device4,
    context: ID3D11DeviceContext4,
    info: DisplayInfo,
    gpu_profiler: D3d11GpuProfiler,
    first_signal_logged: bool,
    last_fault_stage: GpuFaultStage,
    last_ready_completed: Option<u64>,
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
            first_signal_logged: false,
            last_fault_stage: GpuFaultStage::CaptureCopy,
            last_ready_completed: None,
        })
    }

    pub fn info(&self) -> &DisplayInfo {
        &self.info
    }

    pub(crate) fn transport_device(&self) -> ID3D11Device4 {
        self.device.clone()
    }

    pub(crate) fn device_lost(&self) -> bool {
        super::gpu_diagnostics::d3d11_device_lost(&self.device)
    }

    pub(crate) fn device_removed_reason(&self) -> Option<i32> {
        super::gpu_diagnostics::d3d11_device_removed_reason(&self.device)
    }

    pub(crate) fn last_fault_stage(&self) -> GpuFaultStage {
        self.last_fault_stage
    }

    pub(crate) fn last_ready_completed(&self) -> Option<u64> {
        self.last_ready_completed
    }

    pub(crate) fn acquire(&mut self, timeout_ms: u32, qpc: QpcClock) -> Result<CapturePoll> {
        if self.frame_held {
            // SAFETY: this instance tracks that the previous successful acquire is held.
            let release_result = unsafe { self.duplication.ReleaseFrame() };
            self.frame_held = false;
            if let Err(error) = release_result {
                if error.code() == DXGI_ERROR_ACCESS_LOST {
                    self.recreate_duplication()?;
                    return Ok(CapturePoll::NoDesktopUpdate(
                        NoDesktopUpdateReason::AccessLost,
                    ));
                }
                return Err(error).context("failed to release previous DXGI frame");
            }
        }

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let attempt_start_qpc = qpc.now();
        // SAFETY: output parameters point to writable storage and timeout is caller supplied.
        match unsafe {
            self.duplication
                .AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource)
        } {
            Ok(()) => self.frame_held = true,
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                return Ok(CapturePoll::NoDesktopUpdate(NoDesktopUpdateReason::Timeout));
            }
            Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => {
                self.recreate_duplication()?;
                return Ok(CapturePoll::NoDesktopUpdate(
                    NoDesktopUpdateReason::AccessLost,
                ));
            }
            Err(error) => return Err(error).context("failed to acquire DXGI desktop frame"),
        }
        let acquired_qpc = qpc.now();
        if frame_info.LastPresentTime == 0 {
            return Ok(CapturePoll::NoDesktopUpdate(
                NoDesktopUpdateReason::PointerOnly,
            ));
        }
        let resource = resource.context("DXGI returned no desktop resource")?;
        let texture = resource
            .cast::<ID3D11Texture2D>()
            .context("DXGI desktop resource is not a D3D11 texture")?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc points to writable storage for the texture descriptor.
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            bail!("unsupported DXGI pixel format; disable HDR/10-bit output for this build");
        }
        Ok(CapturePoll::Fresh(AcquiredDesktopFrame {
            texture,
            attempt_start_qpc,
            acquired_qpc,
            desktop_size: (desc.Width as usize, desc.Height as usize),
            present_qpc: frame_info.LastPresentTime,
            accumulated_frames: frame_info.AccumulatedFrames,
        }))
    }

    fn copy_center_image(
        &self,
        frame: &AcquiredDesktopFrame,
        destination: &ID3D11Texture2D,
        (width, height): (usize, usize),
    ) -> Result<()> {
        if width == 0 || height == 0 {
            bail!("model input size must be non-zero");
        }
        let mut source_desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: the source texture is live.
        unsafe { frame.texture.GetDesc(&mut source_desc) };
        let (frame_width, frame_height) = (source_desc.Width as usize, source_desc.Height as usize);
        if width > frame_width || height > frame_height {
            bail!(
                "model input {}x{} is larger than captured desktop {}x{}",
                width,
                height,
                frame_width,
                frame_height
            );
        }
        let mut destination_desc =
            windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
        unsafe { destination.GetDesc(&mut destination_desc) };
        if destination_desc.Width as usize != width || destination_desc.Height as usize != height {
            bail!("shared frame texture dimensions do not match model input");
        }
        let (left, top) = super::center_crop_origin((frame_width, frame_height), (width, height));
        let (left, top) = (left as u32, top as u32);
        let source_box = D3D11_BOX {
            left,
            top,
            front: 0,
            right: left + width as u32,
            bottom: top + height as u32,
            back: 1,
        };
        let destination = destination
            .cast::<ID3D11Resource>()
            .context("failed to cast shared destination texture")?;
        let source = frame
            .texture
            .cast::<ID3D11Resource>()
            .context("failed to cast desktop texture to D3D11 resource")?;
        // SAFETY: resources share a device; source_box stays in bounds.
        unsafe {
            self.context.CopySubresourceRegion(
                &destination,
                0,
                0,
                0,
                0,
                &source,
                0,
                Some(&source_box as *const D3D11_BOX),
            );
        }
        Ok(())
    }

    pub(crate) fn copy_center_to(
        &mut self,
        frame: &AcquiredDesktopFrame,
        destination: &ID3D11Texture2D,
        ready_fence: &ID3D11Fence,
        ready_value: u64,
        (width, height): (usize, usize),
        qpc: QpcClock,
    ) -> Result<CaptureSubmit> {
        self.last_fault_stage = GpuFaultStage::CaptureCopy;
        let query_slot = if self.gpu_profiler.sampler.should_measure() {
            self.gpu_profiler.begin(&self.context)
        } else {
            None
        };
        self.copy_center_image(frame, destination, (width, height))?;
        let copy_submitted_qpc = qpc.now();
        tracing::debug!(width, height, "copy_subresource_submitted");
        if let Some(index) = query_slot {
            self.gpu_profiler.end(&self.context, index);
        }
        self.last_fault_stage = GpuFaultStage::CaptureReadySignal;
        let completed_before = unsafe { ready_fence.GetCompletedValue() };
        self.last_ready_completed = Some(completed_before);
        if completed_before == u64::MAX {
            self.last_fault_stage = GpuFaultStage::DeviceRemoved;
            bail!("D3D11 capture-ready fence reports device removed");
        }
        if !self.first_signal_logged {
            tracing::info!(
                value = ready_value,
                completed_before,
                "capture_fence_first_signal"
            );
        }
        if let Err(error) = unsafe { self.context.Signal(ready_fence, ready_value) } {
            let hr = format!("0x{:08X}", error.code().0 as u32);
            tracing::error!(
                value = ready_value,
                completed_before,
                hr,
                "capture_ready_signal_failed"
            );
            return Err(error).context("failed to signal capture-ready fence");
        }
        // The immediate context buffers commands; without a flush the copy and
        // Signal may not reach the GPU while the D3D12 queue already waits on it.
        unsafe { self.context.Flush() };
        tracing::debug!(
            value = ready_value,
            completed_before,
            "capture_ready_signal"
        );
        if !self.first_signal_logged {
            tracing::info!(value = ready_value, "capture_fence_first_signal_ok");
            self.first_signal_logged = true;
        }
        Ok(CaptureSubmit {
            present_qpc: frame.present_qpc,
            attempt_start_qpc: frame.attempt_start_qpc,
            acquired_qpc: frame.acquired_qpc,
            copy_submitted_qpc,
            ready_qpc: qpc.now(),
            desktop_size: frame.desktop_size,
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
        tracing::debug!("DXGI desktop duplication recreated after access loss");
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
            Default::default(),
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
