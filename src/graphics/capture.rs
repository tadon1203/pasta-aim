use std::{slice, time::Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::{
    Foundation::HMODULE,
    Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_1},
        Direct3D11::{
            D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BOX, D3D11_CPU_ACCESS_READ,
            D3D11_CREATE_DEVICE_FLAG, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_QUERY,
            D3D11_QUERY_DATA_TIMESTAMP_DISJOINT, D3D11_QUERY_DESC, D3D11_QUERY_TIMESTAMP,
            D3D11_QUERY_TIMESTAMP_DISJOINT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11Device4,
            ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Query, ID3D11Resource,
            ID3D11Texture2D,
        },
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
        Dxgi::{
            DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
            IDXGIAdapter1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
        },
    },
};
use windows::core::Interface;

use super::{DisplayInfo, select_output};

const GPU_QUERY_RING_SIZE: usize = 16;
const GPU_PROFILE_COLD_FRAMES: u64 = 16;
const GPU_PROFILE_STEADY_STRIDE: u64 = 16;

struct StagingTexture {
    width: usize,
    height: usize,
    texture: ID3D11Texture2D,
}

pub(crate) enum CapturePoll {
    Fresh(AcquiredDesktopFrame),
    NoDesktopUpdate(NoDesktopUpdateReason),
}

pub(crate) struct AcquiredDesktopFrame {
    texture: ID3D11Texture2D,
    acquire_wait_ms: f64,
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

pub(crate) struct MappedCaptureFrame<'a> {
    mapped_ptr: *const u8,
    mapped_len: usize,
    row_pitch: usize,
    width: usize,
    height: usize,
    acquire_wait_ms: f64,
    capture_ms: f64,
    copy_submit_ms: f64,
    map_wait_ms: f64,
    desktop_size: (usize, usize),
    present_qpc: i64,
    accumulated_frames: u32,
    context: &'a ID3D11DeviceContext4,
    texture: &'a ID3D11Texture2D,
}

impl MappedCaptureFrame<'_> {
    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: mapping metadata is validated before this frame is returned.
        unsafe { slice::from_raw_parts(self.mapped_ptr, self.mapped_len) }
    }

    pub(crate) fn row_pitch(&self) -> usize {
        self.row_pitch
    }
    pub(crate) fn acquire_wait_ms(&self) -> f64 {
        self.acquire_wait_ms
    }
    pub(crate) fn capture_ms(&self) -> f64 {
        self.capture_ms
    }
    pub(crate) fn copy_submit_ms(&self) -> f64 {
        self.copy_submit_ms
    }
    pub(crate) fn map_wait_ms(&self) -> f64 {
        self.map_wait_ms
    }
    pub(crate) fn desktop_size(&self) -> (usize, usize) {
        self.desktop_size
    }
    pub(crate) fn present_qpc(&self) -> i64 {
        self.present_qpc
    }
    pub(crate) fn accumulated_frames(&self) -> u32 {
        self.accumulated_frames
    }
}

impl AcquiredDesktopFrame {
    pub(crate) fn present_qpc(&self) -> i64 {
        self.present_qpc
    }
    pub(crate) fn accumulated_frames(&self) -> u32 {
        self.accumulated_frames
    }
}

impl Drop for MappedCaptureFrame<'_> {
    fn drop(&mut self) {
        // SAFETY: this frame owns the sole active Map for subresource zero.
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GpuCopySample {
    pub present_qpc: i64,
    pub gpu_copy_ms: f64,
}

struct GpuQuerySlot {
    disjoint: ID3D11Query,
    before: ID3D11Query,
    after: ID3D11Query,
    present_qpc: i64,
    in_flight: bool,
}

struct D3d11GpuProfiler {
    slots: Vec<GpuQuerySlot>,
    next_slot: usize,
    accepted_frames: u64,
}

impl D3d11GpuProfiler {
    fn new(device: &ID3D11Device4) -> Result<Self> {
        let mut slots = Vec::with_capacity(GPU_QUERY_RING_SIZE);
        for _ in 0..GPU_QUERY_RING_SIZE {
            slots.push(GpuQuerySlot {
                disjoint: create_query(device, D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                before: create_query(device, D3D11_QUERY_TIMESTAMP)?,
                after: create_query(device, D3D11_QUERY_TIMESTAMP)?,
                present_qpc: 0,
                in_flight: false,
            });
        }
        Ok(Self {
            slots,
            next_slot: 0,
            accepted_frames: 0,
        })
    }

    fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.in_flight = false;
        }
        self.next_slot = 0;
        self.accepted_frames = 0;
    }

    fn should_measure(&mut self) -> bool {
        let frame = self.accepted_frames;
        self.accepted_frames = self.accepted_frames.saturating_add(1);
        frame < GPU_PROFILE_COLD_FRAMES
            || (frame >= GPU_PROFILE_COLD_FRAMES
                && (frame - GPU_PROFILE_COLD_FRAMES) % GPU_PROFILE_STEADY_STRIDE == 0)
    }

    fn begin(&mut self, context: &ID3D11DeviceContext4, present_qpc: i64) -> Option<usize> {
        let slot_count = self.slots.len();
        for offset in 0..slot_count {
            let index = (self.next_slot + offset) % slot_count;
            if !self.slots[index].in_flight {
                let slot = &mut self.slots[index];
                slot.present_qpc = present_qpc;
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
                present_qpc: slot.present_qpc,
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
    staging: Option<StagingTexture>,
    info: DisplayInfo,
    gpu_profiler: D3d11GpuProfiler,
}

impl DxgiCapture {
    pub(crate) fn new(adapter_index: u32, output_index: u32) -> Result<Self> {
        tracing::info!(adapter_index, output_index, "initializing DXGI capture");
        let selected = select_output(adapter_index, output_index)?;
        let (device, context) = create_device(&selected.adapter)?;
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
            staging: None,
            info,
            gpu_profiler,
        })
    }

    pub fn info(&self) -> &DisplayInfo {
        &self.info
    }

    pub(crate) fn acquire(&mut self, timeout_ms: u32) -> Result<CapturePoll> {
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
        let acquire_started = Instant::now();
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
        let acquire_wait_ms = acquire_started.elapsed().as_secs_f64() * 1000.0;
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
            acquire_wait_ms,
            desktop_size: (desc.Width as usize, desc.Height as usize),
            present_qpc: frame_info.LastPresentTime,
            accumulated_frames: frame_info.AccumulatedFrames,
        }))
    }

    pub(crate) fn copy_map_center(
        &mut self,
        frame: &AcquiredDesktopFrame,
        width: usize,
        height: usize,
    ) -> Result<MappedCaptureFrame<'_>> {
        if width == 0 || height == 0 {
            bail!("model input size must be non-zero");
        }
        let (frame_width, frame_height) = frame.desktop_size;
        if width > frame_width || height > frame_height {
            bail!(
                "model input {}x{} is larger than captured desktop {}x{}",
                width,
                height,
                frame_width,
                frame_height
            );
        }
        self.ensure_staging_texture(width, height)?;
        let left = ((frame_width - width) / 2) as u32;
        let top = ((frame_height - height) / 2) as u32;
        let source_box = D3D11_BOX {
            left,
            top,
            front: 0,
            right: left + width as u32,
            bottom: top + height as u32,
            back: 1,
        };
        let staging = self
            .staging
            .as_ref()
            .context("staging texture was not created")?;
        let source = frame
            .texture
            .cast::<ID3D11Resource>()
            .context("failed to cast desktop texture to D3D11 resource")?;
        let capture_started = Instant::now();
        let copy_started = Instant::now();
        let query_slot = if self.gpu_profiler.should_measure() {
            self.gpu_profiler.begin(&self.context, frame.present_qpc)
        } else {
            None
        };
        // SAFETY: resources share a device; source_box stays in bounds.
        unsafe {
            self.context.CopySubresourceRegion(
                &staging.texture,
                0,
                0,
                0,
                0,
                &source,
                0,
                Some(&source_box as *const D3D11_BOX),
            );
        }
        let copy_submit_ms = copy_started.elapsed().as_secs_f64() * 1000.0;
        if let Some(index) = query_slot {
            self.gpu_profiler.end(&self.context, index);
        }

        let map_started = Instant::now();
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: staging has CPU read access and is not mapped elsewhere.
        unsafe {
            self.context
                .Map(&staging.texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("failed to map DXGI staging texture")?;
        }
        let map_wait_ms = map_started.elapsed().as_secs_f64() * 1000.0;
        let capture_ms = capture_started.elapsed().as_secs_f64() * 1000.0;
        let row_pitch = mapped.RowPitch as usize;
        let mapped_len = match row_pitch.checked_mul(height) {
            Some(value) => value,
            None => {
                unsafe { self.context.Unmap(&staging.texture, 0) };
                bail!("mapped DXGI byte length overflowed");
            }
        };
        let frame = MappedCaptureFrame {
            mapped_ptr: mapped.pData.cast(),
            mapped_len,
            row_pitch,
            width,
            height,
            acquire_wait_ms: frame.acquire_wait_ms,
            capture_ms,
            copy_submit_ms,
            map_wait_ms,
            desktop_size: frame.desktop_size,
            present_qpc: frame.present_qpc,
            accumulated_frames: frame.accumulated_frames,
            context: &self.context,
            texture: &staging.texture,
        };
        if frame.mapped_ptr.is_null() {
            bail!("D3D11 mapped staging texture returned a null pointer");
        }
        let minimum_row_bytes = frame
            .width
            .checked_mul(4)
            .context("mapped DXGI row size overflowed")?;
        if frame.row_pitch < minimum_row_bytes {
            bail!(
                "mapped DXGI row pitch {} is smaller than required {}",
                frame.row_pitch,
                minimum_row_bytes
            );
        }
        let expected_len = frame
            .row_pitch
            .checked_mul(frame.height)
            .context("mapped DXGI byte length overflowed")?;
        if frame.mapped_len != expected_len {
            bail!("mapped DXGI byte length did not match its row layout");
        }
        Ok(frame)
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

    fn ensure_staging_texture(&mut self, width: usize, height: usize) -> Result<()> {
        if self
            .staging
            .as_ref()
            .is_some_and(|value| value.width == width && value.height == height)
        {
            return Ok(());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width
                .try_into()
                .context("staging width exceeds D3D11 limits")?,
            Height: height
                .try_into()
                .context("staging height exceeds D3D11 limits")?,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: descriptor is initialized and staging resources need no initial data.
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .context("failed to create DXGI staging texture")?;
        }
        let texture = texture.context("D3D11 returned no staging texture")?;
        self.staging = Some(StagingTexture {
            width,
            height,
            texture,
        });
        tracing::debug!(width, height, "DXGI staging texture created");
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
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[D3D_FEATURE_LEVEL_11_1]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .context("failed to create D3D11 device")?;
    let device = device.context("D3D11 returned no device")?.cast()?;
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
