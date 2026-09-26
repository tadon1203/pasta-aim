//! GPU rendering of the overlay: a DXGI flip swap chain bound to the window
//! through DirectComposition (premultiplied alpha, so the desktop shows
//! through), drawn with Direct2D.
//!
//! The device is private to the overlay thread and independent of the capture
//! and inference devices, so drawing never contends with the frame path.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result};
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
                },
                D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET,
                D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
                D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Bitmap1,
                ID2D1DeviceContext, ID2D1Factory1, ID2D1Image, ID2D1SolidColorBrush,
                ID2D1StrokeStyle,
            },
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
                ID3D11Device,
            },
            DirectComposition::{
                DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget,
                IDCompositionVisual,
            },
            Dxgi::{
                Common::{
                    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
                },
                DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter, IDXGIDevice1, IDXGIFactory2,
                IDXGISurface, IDXGISwapChain1,
            },
        },
    },
    core::Interface,
};

use super::layout::Stroke;

const TRANSPARENT: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};

/// Colors for one draw: straight (non-premultiplied) RGB plus opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub line: [u8; 3],
    pub outline: [u8; 3],
    pub opacity: f32,
}

pub struct Renderer {
    context: ID2D1DeviceContext,
    swap_chain: IDXGISwapChain1,
    line_brush: ID2D1SolidColorBrush,
    outline_brush: ID2D1SolidColorBrush,
    // Held so the composition tree stays alive with the swap chain.
    _dcomp_device: IDCompositionDevice,
    _dcomp_target: IDCompositionTarget,
    _dcomp_visual: IDCompositionVisual,
    size: (u32, u32),
}

impl Renderer {
    pub fn new(hwnd: HWND, size: (u32, u32)) -> Result<Self> {
        // SAFETY: plain COM object creation on the overlay thread; every
        // pointer handed to the API outlives its call.
        unsafe {
            let mut d3d_device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                None::<&HMODULE>,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                None,
            )
            .context("failed to create the overlay D3D11 device")?;
            let d3d_device = d3d_device.context("D3D11 returned no overlay device")?;
            let dxgi_device: IDXGIDevice1 = d3d_device.cast()?;
            // Present must never queue up behind the compositor.
            let _ = dxgi_device.SetMaximumFrameLatency(1);

            let factory: IDXGIFactory2 = dxgi_device.GetAdapter()?.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: size.0,
                Height: size.1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: 0,
            };
            let swap_chain = factory
                .CreateSwapChainForComposition(&d3d_device, &desc, None)
                .context("failed to create the overlay swap chain")?;

            let d2d_factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                    .context("failed to create the Direct2D factory")?;
            let d2d_device = d2d_factory.CreateDevice(&dxgi_device)?;
            let context = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let line_brush = context.CreateSolidColorBrush(&TRANSPARENT, None)?;
            let outline_brush = context.CreateSolidColorBrush(&TRANSPARENT, None)?;

            let dcomp_device: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)
                .context("failed to create the DirectComposition device")?;
            let dcomp_target = dcomp_device.CreateTargetForHwnd(hwnd, true)?;
            let dcomp_visual = dcomp_device.CreateVisual()?;
            dcomp_visual.SetContent(&swap_chain)?;
            dcomp_target.SetRoot(&dcomp_visual)?;
            dcomp_device.Commit()?;

            Ok(Self {
                context,
                swap_chain,
                line_brush,
                outline_brush,
                _dcomp_device: dcomp_device,
                _dcomp_target: dcomp_target,
                _dcomp_visual: dcomp_visual,
                size,
            })
        }
    }

    pub fn resize(&mut self, size: (u32, u32)) -> Result<()> {
        if size == self.size {
            return Ok(());
        }
        // SAFETY: no render target references the buffers (draw clears it).
        unsafe {
            self.swap_chain.ResizeBuffers(
                0,
                size.0,
                size.1,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                Default::default(),
            )
        }
        .context("failed to resize the overlay swap chain")?;
        self.size = size;
        Ok(())
    }

    /// Clears the frame and draws `strokes` (already in window coordinates).
    pub fn draw(&mut self, strokes: &[Option<Stroke>], palette: Palette) -> Result<()> {
        // SAFETY: the surface, bitmap and context belong to this renderer, and
        // the target is released before the buffers can be resized or presented.
        unsafe {
            self.line_brush
                .SetColor(&color(palette.line, palette.opacity));
            self.outline_brush
                .SetColor(&color(palette.outline, palette.opacity));

            let surface: IDXGISurface = self.swap_chain.GetBuffer(0)?;
            let properties = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: ManuallyDrop::new(None),
            };
            let target: ID2D1Bitmap1 = self
                .context
                .CreateBitmapFromDxgiSurface(&surface, Some(&properties))?;
            self.context.SetTarget(&target);
            self.context.BeginDraw();
            self.context.Clear(Some(&TRANSPARENT));
            for stroke in strokes.iter().flatten() {
                let brush = if stroke.is_outline {
                    &self.outline_brush
                } else {
                    &self.line_brush
                };
                let rect = D2D_RECT_F {
                    left: stroke.rect.left,
                    top: stroke.rect.top,
                    right: stroke.rect.right,
                    bottom: stroke.rect.bottom,
                };
                self.context
                    .DrawRectangle(&rect, brush, stroke.width, None::<&ID2D1StrokeStyle>);
            }
            let ended = self.context.EndDraw(None, None);
            self.context.SetTarget(None::<&ID2D1Image>);
            ended.context("overlay draw failed")?;
            self.swap_chain
                .Present(0, 0)
                .ok()
                .context("overlay present failed")?;
        }
        Ok(())
    }
}

/// Straight sRGB bytes and opacity to a D2D color.
fn color(rgb: [u8; 3], opacity: f32) -> D2D1_COLOR_F {
    let channel = |value: u8| f32::from(value) / 255.0;
    D2D1_COLOR_F {
        r: channel(rgb[0]),
        g: channel(rgb[1]),
        b: channel(rgb[2]),
        a: opacity,
    }
}
