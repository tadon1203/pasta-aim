use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use win_desktop_duplication::{
    DesktopDuplicationApi, DuplicationApiOptions, co_init, devices::AdapterFactory,
    tex_reader::TextureReader, texture::ColorFormat,
};

pub struct CaptureFrame {
    pub nchw_rgb: Vec<f32>,
    pub capture_ms: f64,
    pub preprocess_ms: f64,
    pub desktop_size: (usize, usize),
}

pub struct DxgiCapture {
    duplication: DesktopDuplicationApi,
    reader: TextureReader,
    bytes: Vec<u8>,
}

impl DxgiCapture {
    pub fn new() -> Result<Self> {
        // COM initialization is thread-local. The engine owns this object and keeps
        // all Desktop Duplication calls on this same worker thread.
        co_init();

        let adapter = AdapterFactory::new()
            .get_adapter_by_idx(0)
            .context("no DXGI adapter 0")?;
        let output = adapter
            .get_display_by_idx(0)
            .context("no DXGI output 0 on adapter 0")?;

        let mut duplication = DesktopDuplicationApi::new(adapter, output).map_err(|error| {
            anyhow!("failed to create DXGI Desktop Duplication session: {error:?}")
        })?;
        duplication.configure(DuplicationApiOptions { skip_cursor: true });
        let (device, context) = duplication.get_device_and_ctx();
        let reader = TextureReader::new(device, context);

        Ok(Self {
            duplication,
            reader,
            bytes: Vec::new(),
        })
    }

    pub fn capture_center(
        &mut self,
        model_width: usize,
        model_height: usize,
    ) -> Result<CaptureFrame> {
        let capture_started = Instant::now();
        let texture = self
            .duplication
            .acquire_next_frame_now()
            .map_err(|error| anyhow!("DXGI did not return a desktop frame: {error:?}"))?;
        let desc = texture.desc();
        let frame_width = desc.width as usize;
        let frame_height = desc.height as usize;
        if desc.format != ColorFormat::ABGR8UNorm {
            bail!(
                "unsupported DXGI pixel format; disable HDR/10-bit output for this minimal build"
            );
        }

        if model_width == 0 || model_height == 0 {
            bail!("model input size must be non-zero");
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

        self.reader
            .get_data(&mut self.bytes, &texture)
            .map_err(|error| anyhow!("failed to read DXGI desktop frame: {error:?}"))?;
        let capture_ms = capture_started.elapsed().as_secs_f64() * 1000.0;

        // TextureReader yields the common SDR DXGI BGRA8 layout. This minimal
        // build intentionally rejects unexpected buffer sizes instead of trying
        // to guess HDR/10-bit formats.
        let expected = frame_width
            .checked_mul(frame_height)
            .and_then(|px| px.checked_mul(4))
            .context("desktop dimensions overflowed")?;
        if self.bytes.len() != expected {
            bail!(
                "unsupported DXGI frame layout: expected {} BGRA8 bytes, received {}. Disable HDR for this minimal build",
                expected,
                self.bytes.len()
            );
        }

        let preprocess_started = Instant::now();
        let left = (frame_width - model_width) / 2;
        let top = (frame_height - model_height) / 2;
        let plane = model_width * model_height;
        let mut nchw = vec![0.0_f32; plane * 3];
        const SCALE: f32 = 1.0 / 255.0;

        for y in 0..model_height {
            let source_y = top + y;
            for x in 0..model_width {
                let source_x = left + x;
                let src = (source_y * frame_width + source_x) * 4;
                let dst = y * model_width + x;

                let b = self.bytes[src] as f32 * SCALE;
                let g = self.bytes[src + 1] as f32 * SCALE;
                let r = self.bytes[src + 2] as f32 * SCALE;

                nchw[dst] = r;
                nchw[plane + dst] = g;
                nchw[plane * 2 + dst] = b;
            }
        }
        let preprocess_ms = preprocess_started.elapsed().as_secs_f64() * 1000.0;

        Ok(CaptureFrame {
            nchw_rgb: nchw,
            capture_ms,
            preprocess_ms,
            desktop_size: (frame_width, frame_height),
        })
    }
}
