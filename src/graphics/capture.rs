use std::{slice, time::Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::{
    Foundation::HMODULE,
    Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_1},
        Direct3D11::{
            D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_MAP_READ,
            D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
            D3D11CreateDevice, ID3D11Device, ID3D11Device4, ID3D11DeviceContext,
            ID3D11DeviceContext4, ID3D11Resource, ID3D11Texture2D,
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

struct StagingTexture {
    width: usize,
    height: usize,
    texture: ID3D11Texture2D,
}

pub(crate) enum CapturePoll<'a> {
    Fresh(MappedCaptureFrame<'a>),
    NoDesktopUpdate,
}

pub(crate) struct MappedCaptureFrame<'a> {
    mapped_ptr: *const u8,
    mapped_len: usize,
    row_pitch: usize,
    width: usize,
    height: usize,
    acquired_at: Instant,
    capture_ms: f64,
    frame_wait_ms: f64,
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

    pub(crate) fn acquired_at(&self) -> Instant {
        self.acquired_at
    }

    pub(crate) fn capture_ms(&self) -> f64 {
        self.capture_ms
    }

    pub(crate) fn frame_wait_ms(&self) -> f64 {
        self.frame_wait_ms
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

impl Drop for MappedCaptureFrame<'_> {
    fn drop(&mut self) {
        // SAFETY: this frame owns the sole active Map for subresource zero.
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}

pub struct DxgiCapture {
    output: IDXGIOutput1,
    duplication: IDXGIOutputDuplication,
    frame_held: bool,
    device: ID3D11Device4,
    context: ID3D11DeviceContext4,
    staging: Option<StagingTexture>,
    info: DisplayInfo,
}

impl DxgiCapture {
    pub(super) fn new(adapter_index: u32, output_index: u32) -> Result<Self> {
        tracing::info!(adapter_index, output_index, "initializing DXGI capture");
        let selected = select_output(adapter_index, output_index)?;
        let (device, context) = create_device(&selected.adapter)?;
        let duplication = create_duplication(&selected.output, &device)?;
        let info = selected.info;

        tracing::info!(adapter_index, output_index, width = info.width, height = info.height, display = %info.display_name, "DXGI capture initialized");
        Ok(Self {
            output: selected.output,
            duplication,
            frame_held: false,
            device,
            context,
            staging: None,
            info,
        })
    }

    pub fn info(&self) -> &DisplayInfo {
        &self.info
    }

    pub(crate) fn try_capture_center(
        &mut self,
        model_width: usize,
        model_height: usize,
    ) -> Result<CapturePoll<'_>> {
        if model_width == 0 || model_height == 0 {
            bail!("model input size must be non-zero");
        }

        let capture_started = Instant::now();
        self.ensure_staging_texture(model_width, model_height)?;
        if !self.prepare_acquire()? {
            return Ok(CapturePoll::NoDesktopUpdate);
        }

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: output parameters point to writable storage and timeout is explicitly zero.
        match unsafe {
            self.duplication
                .AcquireNextFrame(0, &mut frame_info, &mut resource)
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
        let acquired_at = Instant::now();

        if frame_info.LastPresentTime == 0 {
            return Ok(CapturePoll::NoDesktopUpdate);
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

        let frame_width = desc.Width as usize;
        let frame_height = desc.Height as usize;
        if model_width > frame_width || model_height > frame_height {
            bail!(
                "model input {}x{} is larger than captured desktop {}x{}",
                model_width,
                model_height,
                frame_width,
                frame_height
            );
        }

        let frame_wait_ms = 0.0;
        let left = ((frame_width - model_width) / 2) as u32;
        let top = ((frame_height - model_height) / 2) as u32;
        let source_box = D3D11_BOX {
            left,
            top,
            front: 0,
            right: left + model_width as u32,
            bottom: top + model_height as u32,
            back: 1,
        };
        let staging = self
            .staging
            .as_ref()
            .context("staging texture was not created")?;
        let source = texture
            .cast::<ID3D11Resource>()
            .context("failed to cast desktop texture to D3D11 resource")?;
        // SAFETY: resources share a device and format; source_box stays in bounds.
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

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: staging has CPU read access and is not mapped elsewhere.
        unsafe {
            self.context
                .Map(&staging.texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("failed to map DXGI staging texture")?;
        }
        let capture_ms = capture_started.elapsed().as_secs_f64() * 1000.0;
        let row_pitch = mapped.RowPitch as usize;
        let mapped_len = match row_pitch.checked_mul(model_height) {
            Some(mapped_len) => mapped_len,
            None => {
                // SAFETY: Map succeeded and must be paired with exactly one Unmap.
                unsafe { self.context.Unmap(&staging.texture, 0) };
                bail!("mapped DXGI byte length overflowed");
            }
        };
        let frame = MappedCaptureFrame {
            mapped_ptr: mapped.pData.cast(),
            mapped_len,
            row_pitch,
            width: model_width,
            height: model_height,
            acquired_at,
            capture_ms,
            frame_wait_ms,
            desktop_size: (frame_width, frame_height),
            present_qpc: frame_info.LastPresentTime,
            accumulated_frames: frame_info.AccumulatedFrames,
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
        Ok(CapturePoll::Fresh(frame))
    }

    fn prepare_acquire(&mut self) -> Result<bool> {
        if !self.frame_held {
            return Ok(true);
        }

        // SAFETY: this instance tracks that the previous successful acquire is still held.
        let release_result = unsafe { self.duplication.ReleaseFrame() };
        self.frame_held = false;
        match release_result {
            Ok(()) => Ok(true),
            Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => {
                self.recreate_duplication()?;
                Ok(false)
            }
            Err(error) => Err(error).context("failed to release previous DXGI frame"),
        }
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
            .is_some_and(|staging| staging.width == width && staging.height == height)
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
