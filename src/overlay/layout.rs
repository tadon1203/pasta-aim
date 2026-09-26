//! Overlay geometry: where the window sits and how a box becomes strokes.
//!
//! The window covers the model input region plus [`MARGIN`], so box
//! coordinates (which are in model input space) only need that fixed offset.

use crate::{graphics::center_crop_origin, inference::Detection};

/// Extra window space around the input region so thick strokes and outlines
/// at the region edge are not clipped. Covers the widest possible stroke
/// (line 8 + outline 4, each half outside the box edge).
pub const MARGIN: i32 = 16;

/// A box in model input coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl From<&Detection> for BoxRect {
    fn from(detection: &Detection) -> Self {
        let half_width = detection.box_width * 0.5;
        let half_height = detection.box_height * 0.5;
        Self {
            left: detection.center_x - half_width,
            top: detection.center_y - half_height,
            right: detection.center_x + half_width,
            bottom: detection.center_y + half_height,
        }
    }
}

/// The model input region on the virtual desktop, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// The centered `input` crop of a display. `None` when the input does not
    /// fit the display (the capture copy would fail the same way).
    pub fn for_display(
        desktop_origin: (i32, i32),
        desktop_size: (usize, usize),
        input: (usize, usize),
    ) -> Option<Self> {
        if input.0 == 0 || input.1 == 0 || input.0 > desktop_size.0 || input.1 > desktop_size.1 {
            return None;
        }
        let (x, y) = center_crop_origin(desktop_size, input);
        Some(Self {
            left: desktop_origin.0 + i32::try_from(x).ok()?,
            top: desktop_origin.1 + i32::try_from(y).ok()?,
            width: u32::try_from(input.0).ok()?,
            height: u32::try_from(input.1).ok()?,
        })
    }

    /// Window position and size: the region grown by [`MARGIN`] on every side.
    pub fn window_rect(self) -> (i32, i32, u32, u32) {
        (
            self.left - MARGIN,
            self.top - MARGIN,
            self.width + 2 * MARGIN as u32,
            self.height + 2 * MARGIN as u32,
        )
    }
}

/// One rectangle outline to draw, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    pub rect: BoxRect,
    pub width: f32,
    pub is_outline: bool,
}

/// Outline (outer and inner ring) then the box line, in draw order.
///
/// The outline is two separate rings hugging the line instead of one wider
/// stroke underneath, so translucent colors do not blend over each other.
pub fn strokes(rect: BoxRect, line_width: u8, outline_width: u8) -> [Option<Stroke>; 3] {
    let line = f32::from(line_width);
    let outline = f32::from(outline_width);
    // Snap so that both edges of the line land on whole pixels.
    let snap = |value: f32| {
        let offset = if line_width % 2 == 1 { 0.5 } else { 0.0 };
        (value + MARGIN as f32).round() + offset
    };
    let line_rect = BoxRect {
        left: snap(rect.left),
        top: snap(rect.top),
        right: snap(rect.right),
        bottom: snap(rect.bottom),
    };
    let mut result = [None; 3];
    if outline_width > 0 {
        // Ring centers sit half a line plus half an outline away from the edge.
        let distance = (line + outline) * 0.5;
        result[0] = Some(Stroke {
            rect: grow(line_rect, distance),
            width: outline,
            is_outline: true,
        });
        let inner = grow(line_rect, -distance);
        if inner.right > inner.left && inner.bottom > inner.top {
            result[1] = Some(Stroke {
                rect: inner,
                width: outline,
                is_outline: true,
            });
        }
    }
    result[2] = Some(Stroke {
        rect: line_rect,
        width: line,
        is_outline: false,
    });
    result
}

fn grow(rect: BoxRect, amount: f32) -> BoxRect {
    BoxRect {
        left: rect.left - amount,
        top: rect.top - amount,
        right: rect.right + amount,
        bottom: rect.bottom + amount,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> BoxRect {
        BoxRect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn region_is_the_centered_crop_on_the_display() {
        let region = Region::for_display((1920, -100), (2560, 1440), (640, 640)).unwrap();
        assert_eq!((region.left, region.top), (1920 + 960, -100 + 400));
        assert_eq!((region.width, region.height), (640, 640));
        // Odd differences round down, exactly like the capture copy.
        let odd = Region::for_display((0, 0), (1921, 1081), (640, 640)).unwrap();
        assert_eq!((odd.left, odd.top), (640, 220));
    }

    #[test]
    fn region_rejects_inputs_that_do_not_fit() {
        assert!(Region::for_display((0, 0), (1920, 1080), (2000, 640)).is_none());
        assert!(Region::for_display((0, 0), (1920, 1080), (0, 640)).is_none());
    }

    #[test]
    fn window_is_the_region_grown_by_the_margin() {
        let region = Region {
            left: 100,
            top: 50,
            width: 640,
            height: 480,
        };
        assert_eq!(
            region.window_rect(),
            (100 - MARGIN, 50 - MARGIN, 640 + 32, 480 + 32)
        );
    }

    #[test]
    fn box_is_built_from_center_and_size() {
        let detection = Detection {
            center_x: 100.0,
            center_y: 80.0,
            box_width: 40.0,
            box_height: 20.0,
            confidence: 0.9,
            class_id: 0,
        };
        assert_eq!(BoxRect::from(&detection), rect(80.0, 70.0, 120.0, 90.0));
    }

    #[test]
    fn line_without_outline_is_a_single_stroke_snapped_to_pixels() {
        let [outer, inner, line] = strokes(rect(10.2, 20.7, 50.0, 60.0), 2, 0);
        assert!(outer.is_none() && inner.is_none());
        let line = line.unwrap();
        assert_eq!(line.width, 2.0);
        assert!(!line.is_outline);
        // Even widths are centered on whole pixels, odd widths on half pixels.
        let m = MARGIN as f32;
        assert_eq!(line.rect.left, 10.0 + m);
        assert_eq!(line.rect.top, 21.0 + m);
        let odd = strokes(rect(10.0, 20.0, 50.0, 60.0), 3, 0)[2].unwrap();
        assert_eq!(odd.rect.left, 10.5 + m);
    }

    #[test]
    fn outline_rings_touch_the_line_without_overlapping_it() {
        let [outer, inner, line] = strokes(rect(10.0, 10.0, 60.0, 60.0), 4, 2);
        let (outer, inner, line) = (outer.unwrap(), inner.unwrap(), line.unwrap());
        assert!(outer.is_outline && inner.is_outline);
        // Line covers [e - 2, e + 2]; the outer ring covers [e + 2, e + 4].
        let edge = line.rect.right;
        assert_eq!(
            outer.rect.right - outer.width * 0.5,
            edge + line.width * 0.5
        );
        assert_eq!(
            inner.rect.right + inner.width * 0.5,
            edge - line.width * 0.5
        );
    }

    #[test]
    fn tiny_boxes_drop_the_inner_ring() {
        let [outer, inner, line] = strokes(rect(10.0, 10.0, 14.0, 14.0), 4, 2);
        assert!(outer.is_some());
        assert!(inner.is_none());
        assert!(line.is_some());
    }
}
