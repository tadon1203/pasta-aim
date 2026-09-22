use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use win_desktop_duplication::{
    DesktopDuplicationApi, DuplicationApiOptions,
    devices::AdapterFactory,
    tex_reader::TextureReader,
    texture::{ColorFormat, Texture},
};
use windows::Win32::Graphics::{
    Direct3D11::{
        D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device4, ID3D11DeviceContext4,
    },
    Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
};

use super::DisplayInfo;

pub struct CaptureFrame {
    pub acquired_at: Instant,
    pub frame_wait_ms: f64,
    pub capture_ms: f64,
    pub desktop_size: (usize, usize),
}

struct RegionTexture {
    width: usize,
    height: usize,
    texture: Texture,
}

pub struct DxgiCapture {
    duplication: DesktopDuplicationApi,
    device: ID3D11Device4,
    context: ID3D11DeviceContext4,
    reader: TextureReader,
    region: Option<RegionTexture>,
    bytes: Vec<u8>,
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
        let reader = TextureReader::new(device.clone(), context.clone());

        tracing::info!(adapter_index, output_index, width = info.width, height = info.height, display = %info.display_name, "DXGI capture initialized");
        Ok(Self {
            duplication,
            device,
            context,
            reader,
            region: None,
            bytes: Vec::new(),
            info,
        })
    }

    pub fn info(&self) -> &DisplayInfo {
        &self.info
    }

    pub fn pixels(&self) -> &[u8] {
        &self.bytes
    }

    pub fn capture_center(
        &mut self,
        model_width: usize,
        model_height: usize,
    ) -> Result<CaptureFrame> {
        if model_width == 0 || model_height == 0 {
            bail!("model input size must be non-zero");
        }

        // Cadence is controlled exclusively by the worker's monotonic FPS
        // deadline. Do not wait for display VSync here: it aliases requested
        // FPS with the monitor refresh rate and causes visible beat patterns.
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

        self.ensure_region_texture(model_width, model_height)?;
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
        let region = self
            .region
            .as_ref()
            .context("region texture was not created")?;
        // SAFETY: both resources belong to the same D3D11 device, use compatible BGRA8
        // formats, and source_box was checked to stay inside the acquired desktop texture.
        unsafe {
            self.context.CopySubresourceRegion(
                region.texture.as_raw_ref(),
                0,
                0,
                0,
                0,
                texture.as_raw_ref(),
                0,
                Some(&source_box as *const D3D11_BOX),
            );
        }
        self.reader
            .get_data(&mut self.bytes, &region.texture)
            .map_err(|error| anyhow!("failed to read DXGI center region: {error:?}"))?;
        let capture_ms = capture_started.elapsed().as_secs_f64() * 1000.0;

        let expected = model_width
            .checked_mul(model_height)
            .and_then(|px| px.checked_mul(4))
            .context("model dimensions overflowed")?;
        if self.bytes.len() != expected {
            bail!(
                "unsupported DXGI region layout: expected {expected} BGRA8 bytes, received {}",
                self.bytes.len()
            );
        }

        Ok(CaptureFrame {
            acquired_at,
            frame_wait_ms,
            capture_ms,
            desktop_size: (frame_width, frame_height),
        })
    }

    fn ensure_region_texture(&mut self, width: usize, height: usize) -> Result<()> {
        if self
            .region
            .as_ref()
            .is_some_and(|region| region.width == width && region.height == height)
        {
            return Ok(());
        }

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width as u32,
            Height: height as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: 0,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: desc is fully initialized and no initial data is supplied for a DEFAULT texture.
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .context("failed to create DXGI center-region texture")?;
        }
        let texture = texture.context("D3D11 returned no center-region texture")?;
        self.region = Some(RegionTexture {
            width,
            height,
            texture: Texture::new(texture),
        });
        self.bytes.clear();
        tracing::debug!(width, height, "DXGI capture region texture created");
        Ok(())
    }
}
