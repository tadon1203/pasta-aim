use std::{slice, time::Instant};

use anyhow::{Context, Result, anyhow, bail};
use win_desktop_duplication::{
    DesktopDuplicationApi, DuplicationApiOptions, devices::AdapterFactory, texture::ColorFormat,
};
use windows::Win32::Graphics::{
    Direct3D11::{
        D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Device4, ID3D11DeviceContext4,
        ID3D11Texture2D,
    },
    Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
};

use super::DisplayInfo;

struct StagingTexture {
    width: usize,
    height: usize,
    texture: ID3D11Texture2D,
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
}

impl Drop for MappedCaptureFrame<'_> {
    fn drop(&mut self) {
        // SAFETY: this frame owns the sole active Map for subresource zero.
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}

pub struct DxgiCapture {
    duplication: DesktopDuplicationApi,
    device: ID3D11Device4,
    context: ID3D11DeviceContext4,
    staging: Option<StagingTexture>,
    info: DisplayInfo,
}

impl DxgiCapture {
    pub(super) fn new(adapter_index: u32, output_index: u32) -> Result<Self> {
        tracing::info!(adapter_index, output_index, "initializing DXGI capture");
        let factory = AdapterFactory::new();
        let adapter = factory
            .get_adapter_by_idx(adapter_index)
            .with_context(|| format!("no DXGI adapter {adapter_index}"))?;
        let adapter_name = adapter.name();
        let output = adapter
            .get_display_by_idx(output_index)
            .with_context(|| format!("no DXGI output {output_index} on adapter {adapter_index}"))?;
        let mode = output
            .get_current_display_mode()
            .map_err(|error| anyhow!("failed to query selected display mode: {error:?}"))?;
        let refresh_hz = if mode.refresh_den == 0 {
            0.0
        } else {
            f64::from(mode.refresh_num) / f64::from(mode.refresh_den)
        };
        let info = DisplayInfo {
            adapter_index,
            output_index,
            adapter_name,
            display_name: output.name(),
            width: mode.width,
            height: mode.height,
            refresh_hz,
        };

        let mut duplication =
            DesktopDuplicationApi::new(adapter, output.clone()).map_err(|error| {
                anyhow!("failed to create DXGI Desktop Duplication session: {error:?}")
            })?;
        duplication.configure(DuplicationApiOptions { skip_cursor: true });
        let (device, context) = duplication.get_device_and_ctx();

        tracing::info!(adapter_index, output_index, width = info.width, height = info.height, display = %info.display_name, "DXGI capture initialized");
        Ok(Self {
            duplication,
            device,
            context,
            staging: None,
            info,
        })
    }

    pub fn info(&self) -> &DisplayInfo {
        &self.info
    }

    pub(crate) fn capture_center(
        &mut self,
        model_width: usize,
        model_height: usize,
    ) -> Result<MappedCaptureFrame<'_>> {
        if model_width == 0 || model_height == 0 {
            bail!("model input size must be non-zero");
        }

        // Cadence is controlled exclusively by the worker's monotonic FPS deadline.
        let frame_wait_ms = 0.0;
        let capture_started = Instant::now();
        let texture = self
            .duplication
            .acquire_next_frame_now()
            .map_err(|error| anyhow!("DXGI did not return a desktop frame: {error:?}"))?;
        let acquired_at = Instant::now();
        let desc = texture.desc();
        let frame_width = desc.width as usize;
        let frame_height = desc.height as usize;
        if desc.format != ColorFormat::ABGR8UNorm {
            bail!("unsupported DXGI pixel format; disable HDR/10-bit output for this build");
        }
        if model_width > frame_width || model_height > frame_height {
            bail!(
                "model input {}x{} is larger than captured desktop {}x{}",
                model_width,
                model_height,
                frame_width,
                frame_height
            );
        }

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
        self.ensure_staging_texture(model_width, model_height)?;
        let staging = self
            .staging
            .as_ref()
            .context("staging texture was not created")?;
        // SAFETY: resources share a device and format; source_box stays in bounds.
        unsafe {
            self.context.CopySubresourceRegion(
                &staging.texture,
                0,
                0,
                0,
                0,
                texture.as_raw_ref(),
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
