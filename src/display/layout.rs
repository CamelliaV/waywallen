//! Pure-function layout module: turn a `(texture_size, display_size,
//! fillmode, align)` tuple into the `source_rect` / `dest_rect` /
//! `clear_color` triple the wire-level `set_config` event carries.
//!
//! Also hosts `display_point_to_texture` — the inverse mapping used
//! by pointer-event forwarding to translate display-local pixel
//! coordinates back into renderer-texture pixel coordinates.

use serde::{Deserialize, Serialize};

use crate::scheduler::ProjectedConfig;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FillMode {
    Stretched,
    PreserveAspectFit,
    #[default]
    PreserveAspectCrop,
    Centered,
}

/// Buffer-side rotation, expressed as a clockwise turn of the displayed
/// image. Wire-mapped onto `wl_output.transform` 0..3 (no flipped
/// variants); the compositor compensates for the declared pre-rotation
/// so the user sees the wallpaper rotated CW by this much.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    #[default]
    Normal,
    Cw90,
    Cw180,
    Cw270,
}

impl Rotation {
    /// `wl_output.transform` value matching this rotation. The
    /// compositor reads this from `set_buffer_transform` as "the buffer
    /// is pre-rotated by N° CCW", which makes the on-screen image
    /// appear rotated N° CW.
    pub fn to_wl_transform(self) -> u32 {
        match self {
            Rotation::Normal => 0,
            Rotation::Cw90 => 1,
            Rotation::Cw180 => 2,
            Rotation::Cw270 => 3,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Align {
    fn h_factor(self) -> f32 {
        match self {
            Align::TopLeft | Align::Left | Align::BottomLeft => 0.0,
            Align::Top | Align::Center | Align::Bottom => 0.5,
            Align::TopRight | Align::Right | Align::BottomRight => 1.0,
        }
    }
    fn v_factor(self) -> f32 {
        match self {
            Align::TopLeft | Align::Top | Align::TopRight => 0.0,
            Align::Left | Align::Center | Align::Right => 0.5,
            Align::BottomLeft | Align::Bottom | Align::BottomRight => 1.0,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct LayoutInput {
    pub tex_w: f32,
    pub tex_h: f32,
    pub disp_w: f32,
    pub disp_h: f32,
    pub fillmode: FillMode,
    pub align: Align,
    pub clear_rgba: [f32; 4],
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LayoutOutput {
    /// Source rect in texture pixels: (x, y, w, h).
    pub source: (f32, f32, f32, f32),
    /// Destination rect in display pixels: (x, y, w, h).
    pub dest: (f32, f32, f32, f32),
    /// Background fill color (RGBA, sRGB straight alpha).
    pub clear_rgba: [f32; 4],
}

/// Resolve one layout. Pure; never panics. Degenerate inputs
/// (`tex_w/h <= 0` or `disp_w/h <= 0`) collapse to a Stretched output
/// that the consumer will silently no-op.
pub fn compute(i: LayoutInput) -> LayoutOutput {
    if i.tex_w <= 0.0 || i.tex_h <= 0.0 || i.disp_w <= 0.0 || i.disp_h <= 0.0 {
        return LayoutOutput {
            source: (0.0, 0.0, i.tex_w.max(0.0), i.tex_h.max(0.0)),
            dest: (0.0, 0.0, i.disp_w.max(0.0), i.disp_h.max(0.0)),
            clear_rgba: i.clear_rgba,
        };
    }

    match i.fillmode {
        FillMode::Stretched => LayoutOutput {
            source: (0.0, 0.0, i.tex_w, i.tex_h),
            dest: (0.0, 0.0, i.disp_w, i.disp_h),
            clear_rgba: i.clear_rgba,
        },

        FillMode::PreserveAspectFit => {
            let scale = (i.disp_w / i.tex_w).min(i.disp_h / i.tex_h);
            let dw = i.tex_w * scale;
            let dh = i.tex_h * scale;
            let dx = (i.disp_w - dw) * i.align.h_factor();
            let dy = (i.disp_h - dh) * i.align.v_factor();
            LayoutOutput {
                source: (0.0, 0.0, i.tex_w, i.tex_h),
                dest: (dx, dy, dw, dh),
                clear_rgba: i.clear_rgba,
            }
        }

        FillMode::PreserveAspectCrop => {
            // Pick the source-side rect that, when stretched to fill
            // the display, preserves aspect. The cropped axis is
            // positioned by `align`.
            let scale = (i.disp_w / i.tex_w).max(i.disp_h / i.tex_h);
            let sw = i.disp_w / scale;
            let sh = i.disp_h / scale;
            let sx = (i.tex_w - sw) * i.align.h_factor();
            let sy = (i.tex_h - sh) * i.align.v_factor();
            LayoutOutput {
                source: (sx, sy, sw, sh),
                dest: (0.0, 0.0, i.disp_w, i.disp_h),
                clear_rgba: i.clear_rgba,
            }
        }

        FillMode::Centered => {
            // 1:1 pixel display. If the texture is smaller than the
            // display on a given axis, place it inside according to
            // `align` and letterbox the rest. If larger, crop the
            // texture according to `align`.
            let (sx, sw, dx, dw) = axis_centered(i.tex_w, i.disp_w, i.align.h_factor());
            let (sy, sh, dy, dh) = axis_centered(i.tex_h, i.disp_h, i.align.v_factor());
            LayoutOutput {
                source: (sx, sy, sw, sh),
                dest: (dx, dy, dw, dh),
                clear_rgba: i.clear_rgba,
            }
        }
    }
}

/// Map a display-local point to renderer-texture-local pixel coordinates,
/// using the active `ProjectedConfig` (`source_*` in tex space, `dest_*`
/// in display space, `transform` in `wl_output.transform` semantics).
///
/// Returns `None` when the point falls outside `dest_rect` — the caller
/// should drop the event so renderer state isn't poked from the
/// letterbox/pillarbox region.
///
/// `transform` values:
///   0 normal, 1 = 90° CCW, 2 = 180°, 3 = 270° CCW,
///   4 flipped, 5 flipped+90°, 6 flipped+180°, 7 flipped+270°.
pub fn display_point_to_texture(
    disp_x: f32,
    disp_y: f32,
    cfg: &ProjectedConfig,
) -> Option<(f32, f32)> {
    if cfg.dest_w <= 0.0 || cfg.dest_h <= 0.0 {
        return None;
    }
    let u = (disp_x - cfg.dest_x) / cfg.dest_w;
    let v = (disp_y - cfg.dest_y) / cfg.dest_h;
    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
        return None;
    }
    // Inverse of wl_output.transform on the unit square. Buffer→display
    // is the forward; here we go display→buffer.
    let (uu, vv) = match cfg.transform {
        0 => (u, v),
        1 => (1.0 - v, u),
        2 => (1.0 - u, 1.0 - v),
        3 => (v, 1.0 - u),
        4 => (1.0 - u, v),
        5 => (v, u),
        6 => (u, 1.0 - v),
        7 => (1.0 - v, 1.0 - u),
        _ => (u, v),
    };
    Some((
        cfg.source_x + uu * cfg.source_w,
        cfg.source_y + vv * cfg.source_h,
    ))
}

/// One axis of `Centered`. Returns `(source_off, source_len, dest_off, dest_len)`.
fn axis_centered(tex: f32, disp: f32, factor: f32) -> (f32, f32, f32, f32) {
    if tex <= disp {
        // Texture fits — place fully inside the display.
        let dest_len = tex;
        let dest_off = (disp - tex) * factor;
        (0.0, tex, dest_off, dest_len)
    } else {
        // Texture is larger than display — crop a viewport of `disp`
        // pixels out of the texture, positioned by `factor`.
        let src_off = (tex - disp) * factor;
        (src_off, disp, 0.0, disp)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn input(tex: (f32, f32), disp: (f32, f32), fillmode: FillMode, align: Align) -> LayoutInput {
        LayoutInput {
            tex_w: tex.0,
            tex_h: tex.1,
            disp_w: disp.0,
            disp_h: disp.1,
            fillmode,
            align,
            clear_rgba: [0.0, 0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn stretched_is_identity_regardless_of_align() {
        let out = compute(input(
            (1920.0, 1080.0),
            (1280.0, 720.0),
            FillMode::Stretched,
            Align::TopLeft,
        ));
        assert_eq!(out.source, (0.0, 0.0, 1920.0, 1080.0));
        assert_eq!(out.dest, (0.0, 0.0, 1280.0, 720.0));
        let out2 = compute(input(
            (1920.0, 1080.0),
            (1280.0, 720.0),
            FillMode::Stretched,
            Align::BottomRight,
        ));
        assert_eq!(out, out2);
    }

    #[test]
    fn fit_wider_texture_letterboxes_top_bottom() {
        // 16:9 texture into 4:3 display => bars top/bottom, dest_w == disp_w
        let out = compute(input(
            (1920.0, 1080.0),
            (800.0, 600.0),
            FillMode::PreserveAspectFit,
            Align::Center,
        ));
        assert_eq!(out.source, (0.0, 0.0, 1920.0, 1080.0));
        // scale = min(800/1920, 600/1080) = min(0.4167, 0.5556) = 0.4167
        // dest_w = 1920 * 0.4167 = 800; dest_h = 1080 * 0.4167 = 450
        // dy = (600 - 450) * 0.5 = 75
        assert!((out.dest.0 - 0.0).abs() < 1e-3);
        assert!((out.dest.1 - 75.0).abs() < 1e-3);
        assert!((out.dest.2 - 800.0).abs() < 1e-3);
        assert!((out.dest.3 - 450.0).abs() < 1e-3);
    }

    #[test]
    fn fit_top_left_align_pins_to_corner() {
        let out = compute(input(
            (1920.0, 1080.0),
            (800.0, 600.0),
            FillMode::PreserveAspectFit,
            Align::TopLeft,
        ));
        assert!((out.dest.0 - 0.0).abs() < 1e-3);
        assert!((out.dest.1 - 0.0).abs() < 1e-3);
    }

    #[test]
    fn crop_wider_texture_crops_horizontally() {
        // 16:9 tex into 4:3 disp: scale = max(800/1920, 600/1080) = max(0.417, 0.556) = 0.556
        // sw = 800/0.556 = 1440, sh = 600/0.556 = 1080
        // sx = (1920-1440)*0.5 = 240, sy = 0
        let out = compute(input(
            (1920.0, 1080.0),
            (800.0, 600.0),
            FillMode::PreserveAspectCrop,
            Align::Center,
        ));
        assert!((out.source.0 - 240.0).abs() < 1e-3);
        assert!((out.source.1 - 0.0).abs() < 1e-3);
        assert!((out.source.2 - 1440.0).abs() < 1e-3);
        assert!((out.source.3 - 1080.0).abs() < 1e-3);
        assert_eq!(out.dest, (0.0, 0.0, 800.0, 600.0));
    }

    #[test]
    fn crop_top_left_align_keeps_top_left_of_texture() {
        let out = compute(input(
            (1920.0, 1080.0),
            (800.0, 600.0),
            FillMode::PreserveAspectCrop,
            Align::TopLeft,
        ));
        assert!((out.source.0 - 0.0).abs() < 1e-3);
        assert!((out.source.1 - 0.0).abs() < 1e-3);
    }

    #[test]
    fn centered_smaller_texture_letterboxes_around_native_size() {
        // 800x600 tex into 1920x1080 disp, Center align.
        // dest_x = (1920-800)*0.5 = 560, dest_y = (1080-600)*0.5 = 240
        let out = compute(input(
            (800.0, 600.0),
            (1920.0, 1080.0),
            FillMode::Centered,
            Align::Center,
        ));
        assert_eq!(out.source, (0.0, 0.0, 800.0, 600.0));
        assert!((out.dest.0 - 560.0).abs() < 1e-3);
        assert!((out.dest.1 - 240.0).abs() < 1e-3);
        assert!((out.dest.2 - 800.0).abs() < 1e-3);
        assert!((out.dest.3 - 600.0).abs() < 1e-3);
    }

    #[test]
    fn centered_larger_texture_crops_to_display_pixel_for_pixel() {
        // 4000x3000 tex into 1920x1080 disp, Center align.
        // sx = (4000-1920)*0.5 = 1040, sy = (3000-1080)*0.5 = 960, sw=1920, sh=1080
        let out = compute(input(
            (4000.0, 3000.0),
            (1920.0, 1080.0),
            FillMode::Centered,
            Align::Center,
        ));
        assert!((out.source.0 - 1040.0).abs() < 1e-3);
        assert!((out.source.1 - 960.0).abs() < 1e-3);
        assert!((out.source.2 - 1920.0).abs() < 1e-3);
        assert!((out.source.3 - 1080.0).abs() < 1e-3);
        assert_eq!(out.dest, (0.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn centered_top_left_pins_smaller_texture_to_corner() {
        let out = compute(input(
            (800.0, 600.0),
            (1920.0, 1080.0),
            FillMode::Centered,
            Align::TopLeft,
        ));
        assert_eq!(out.dest, (0.0, 0.0, 800.0, 600.0));
    }

    #[test]
    fn degenerate_zero_input_does_not_panic() {
        let out = compute(input(
            (0.0, 0.0),
            (1920.0, 1080.0),
            FillMode::PreserveAspectFit,
            Align::Center,
        ));
        assert_eq!(out.dest, (0.0, 0.0, 1920.0, 1080.0));
        let out = compute(input(
            (1920.0, 1080.0),
            (0.0, 0.0),
            FillMode::PreserveAspectFit,
            Align::Center,
        ));
        assert_eq!(out.source, (0.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn equal_aspect_fit_and_crop_match_stretched() {
        // 16:9 into 16:9: identity for all three modes
        let s = compute(input(
            (1920.0, 1080.0),
            (3840.0, 2160.0),
            FillMode::Stretched,
            Align::Center,
        ));
        let f = compute(input(
            (1920.0, 1080.0),
            (3840.0, 2160.0),
            FillMode::PreserveAspectFit,
            Align::Center,
        ));
        let c = compute(input(
            (1920.0, 1080.0),
            (3840.0, 2160.0),
            FillMode::PreserveAspectCrop,
            Align::Center,
        ));
        assert_eq!(s.dest, f.dest);
        assert_eq!(s.dest, c.dest);
        assert_eq!(s.source, f.source);
        assert_eq!(s.source, c.source);
    }

    // -----------------------------------------------------------------
    // display_point_to_texture
    // -----------------------------------------------------------------

    fn cfg(
        source: (f32, f32, f32, f32),
        dest: (f32, f32, f32, f32),
        transform: u32,
    ) -> ProjectedConfig {
        ProjectedConfig {
            config_generation: 1,
            source_x: source.0,
            source_y: source.1,
            source_w: source.2,
            source_h: source.3,
            dest_x: dest.0,
            dest_y: dest.1,
            dest_w: dest.2,
            dest_h: dest.3,
            transform,
            clear_rgba: [0.0, 0.0, 0.0, 1.0],
        }
    }

    fn approx(a: (f32, f32), b: (f32, f32)) {
        let eps = 1e-3;
        assert!(
            (a.0 - b.0).abs() < eps && (a.1 - b.1).abs() < eps,
            "expected {b:?}, got {a:?}",
        );
    }

    #[test]
    fn point_identity_stretched_same_size() {
        let c = cfg((0.0, 0.0, 1920.0, 1080.0), (0.0, 0.0, 1920.0, 1080.0), 0);
        approx(
            display_point_to_texture(100.0, 50.0, &c).unwrap(),
            (100.0, 50.0),
        );
        approx(display_point_to_texture(0.0, 0.0, &c).unwrap(), (0.0, 0.0));
        approx(
            display_point_to_texture(1920.0, 1080.0, &c).unwrap(),
            (1920.0, 1080.0),
        );
    }

    #[test]
    fn point_stretched_4k_to_1080p() {
        // 4K texture stretched onto a 1080p display.
        let c = cfg((0.0, 0.0, 3840.0, 2160.0), (0.0, 0.0, 1920.0, 1080.0), 0);
        approx(
            display_point_to_texture(960.0, 540.0, &c).unwrap(),
            (1920.0, 1080.0),
        );
        approx(display_point_to_texture(0.0, 0.0, &c).unwrap(), (0.0, 0.0));
    }

    #[test]
    fn point_aspect_fit_letterbox_drops_in_bar() {
        // 1920x1080 texture into 800x600 display, fit -> dest (0, 75, 800, 450).
        let layout = compute(input(
            (1920.0, 1080.0),
            (800.0, 600.0),
            FillMode::PreserveAspectFit,
            Align::Center,
        ));
        let c = cfg(layout.source, layout.dest, 0);
        // Inside picture: center maps to texture center.
        approx(
            display_point_to_texture(400.0, 300.0, &c).unwrap(),
            (960.0, 540.0),
        );
        // Top-left of the visible picture (0, 75) maps to texture (0, 0).
        approx(display_point_to_texture(0.0, 75.0, &c).unwrap(), (0.0, 0.0));
        // In the top letterbox bar -> dropped.
        assert!(display_point_to_texture(400.0, 10.0, &c).is_none());
        // In the bottom letterbox bar -> dropped.
        assert!(display_point_to_texture(400.0, 590.0, &c).is_none());
    }

    #[test]
    fn point_aspect_crop_maps_into_visible_window() {
        // 1920x1080 tex into 800x600 disp, crop center -> source (240, 0, 1440, 1080),
        // dest (0, 0, 800, 600).
        let layout = compute(input(
            (1920.0, 1080.0),
            (800.0, 600.0),
            FillMode::PreserveAspectCrop,
            Align::Center,
        ));
        let c = cfg(layout.source, layout.dest, 0);
        // Display center maps to texture center.
        approx(
            display_point_to_texture(400.0, 300.0, &c).unwrap(),
            (960.0, 540.0),
        );
        // Top-left of display maps to top-left of the cropped source rect.
        approx(
            display_point_to_texture(0.0, 0.0, &c).unwrap(),
            (240.0, 0.0),
        );
    }

    #[test]
    fn point_outside_dest_rect_returns_none() {
        // Dest offset by (100, 50), 200x100 wide.
        let c = cfg((0.0, 0.0, 100.0, 100.0), (100.0, 50.0, 200.0, 100.0), 0);
        assert!(display_point_to_texture(50.0, 75.0, &c).is_none()); // left of dest
        assert!(display_point_to_texture(150.0, 25.0, &c).is_none()); // above dest
        assert!(display_point_to_texture(350.0, 75.0, &c).is_none()); // right of dest
        assert!(display_point_to_texture(150.0, 200.0, &c).is_none()); // below dest
        assert!(display_point_to_texture(150.0, 100.0, &c).is_some()); // inside dest
    }

    #[test]
    fn point_transform_90_ccw_inverse_corner_mapping() {
        // Forward: 90° CCW rotation of buffer onto display.
        //   buffer A=top-left, B=top-right, C=bottom-left, D=bottom-right
        //   end up on display as: B=top-left, D=top-right, A=bottom-left,
        //   C=bottom-right. Inverse maps display corners back to buffer:
        // Buffer 100x200 (tall) -> 200x100 (wide) display.
        let c = cfg((0.0, 0.0, 100.0, 200.0), (0.0, 0.0, 200.0, 100.0), 1);
        // display top-left (0, 0) -> buffer top-right (100, 0)
        approx(
            display_point_to_texture(0.0, 0.0, &c).unwrap(),
            (100.0, 0.0),
        );
        // display top-right (200, 0) -> buffer bottom-right (100, 200)
        approx(
            display_point_to_texture(200.0, 0.0, &c).unwrap(),
            (100.0, 200.0),
        );
        // display bottom-right (200, 100) -> buffer bottom-left (0, 200)
        approx(
            display_point_to_texture(200.0, 100.0, &c).unwrap(),
            (0.0, 200.0),
        );
        // display bottom-left (0, 100) -> buffer top-left (0, 0)
        approx(
            display_point_to_texture(0.0, 100.0, &c).unwrap(),
            (0.0, 0.0),
        );
    }

    #[test]
    fn point_transform_180_inverse() {
        let c = cfg((0.0, 0.0, 1920.0, 1080.0), (0.0, 0.0, 1920.0, 1080.0), 2);
        approx(
            display_point_to_texture(0.0, 0.0, &c).unwrap(),
            (1920.0, 1080.0),
        );
        approx(
            display_point_to_texture(1920.0, 1080.0, &c).unwrap(),
            (0.0, 0.0),
        );
        approx(
            display_point_to_texture(960.0, 540.0, &c).unwrap(),
            (960.0, 540.0),
        );
    }

    #[test]
    fn point_transform_270_ccw_corner_mapping() {
        // 270° CCW = 90° CW: buffer C=bottom-left ends up at display
        // top-left. Inverse:
        // Buffer 100x200 (tall) -> 200x100 (wide) display.
        let c270 = cfg((0.0, 0.0, 100.0, 200.0), (0.0, 0.0, 200.0, 100.0), 3);
        // display top-left -> buffer bottom-left (0, 200)
        approx(
            display_point_to_texture(0.0, 0.0, &c270).unwrap(),
            (0.0, 200.0),
        );
        // display top-right -> buffer top-left (0, 0)
        approx(
            display_point_to_texture(200.0, 0.0, &c270).unwrap(),
            (0.0, 0.0),
        );
        // Sanity: differs from transform=1 at the same corner.
        let c90 = cfg((0.0, 0.0, 100.0, 200.0), (0.0, 0.0, 200.0, 100.0), 1);
        let p1 = display_point_to_texture(0.0, 0.0, &c90).unwrap();
        let p3 = display_point_to_texture(0.0, 0.0, &c270).unwrap();
        assert_ne!(p1, p3);
    }

    #[test]
    fn point_transform_flipped_horizontal() {
        let c = cfg((0.0, 0.0, 1920.0, 1080.0), (0.0, 0.0, 1920.0, 1080.0), 4);
        // Horizontal flip: x mirrors, y stays.
        approx(
            display_point_to_texture(0.0, 100.0, &c).unwrap(),
            (1920.0, 100.0),
        );
        approx(
            display_point_to_texture(1920.0, 100.0, &c).unwrap(),
            (0.0, 100.0),
        );
        approx(
            display_point_to_texture(960.0, 540.0, &c).unwrap(),
            (960.0, 540.0),
        );
    }

    #[test]
    fn point_transform_5_flipped_90_swaps_axes() {
        // flipped + 90° CCW reduces to swap-axes on the unit square.
        let c = cfg((0.0, 0.0, 100.0, 200.0), (0.0, 0.0, 200.0, 100.0), 5);
        // (u, v) = (0.5, 0.5) (display center) → buffer (0.5, 0.5) -> (50, 100)
        approx(
            display_point_to_texture(100.0, 50.0, &c).unwrap(),
            (50.0, 100.0),
        );
        // (u, v) = (0, 0) → (0, 0)
        approx(display_point_to_texture(0.0, 0.0, &c).unwrap(), (0.0, 0.0));
        // (u, v) = (1, 1) → (1, 1) -> (100, 200)
        approx(
            display_point_to_texture(200.0, 100.0, &c).unwrap(),
            (100.0, 200.0),
        );
    }

    #[test]
    fn point_transform_6_flipped_180_is_vertical_flip() {
        // flipped + 180° on the unit square = (u, 1-v): vertical flip.
        let c = cfg((0.0, 0.0, 1920.0, 1080.0), (0.0, 0.0, 1920.0, 1080.0), 6);
        approx(
            display_point_to_texture(100.0, 0.0, &c).unwrap(),
            (100.0, 1080.0),
        );
        approx(
            display_point_to_texture(100.0, 1080.0, &c).unwrap(),
            (100.0, 0.0),
        );
    }

    #[test]
    fn point_transform_7_flipped_270() {
        // flipped + 270° CCW on the unit square = (1-v, 1-u).
        let c = cfg((0.0, 0.0, 100.0, 200.0), (0.0, 0.0, 200.0, 100.0), 7);
        // Display (0, 0) -> u=0, v=0 -> (1-0, 1-0) = (1, 1) -> buffer (100, 200)
        approx(
            display_point_to_texture(0.0, 0.0, &c).unwrap(),
            (100.0, 200.0),
        );
        // Display (200, 100) -> u=1, v=1 -> (0, 0) -> buffer (0, 0)
        approx(
            display_point_to_texture(200.0, 100.0, &c).unwrap(),
            (0.0, 0.0),
        );
    }

    #[test]
    fn point_unknown_transform_falls_back_to_identity() {
        // Defensive: unknown transform shouldn't panic.
        let c = cfg((0.0, 0.0, 1920.0, 1080.0), (0.0, 0.0, 1920.0, 1080.0), 99);
        approx(
            display_point_to_texture(100.0, 50.0, &c).unwrap(),
            (100.0, 50.0),
        );
    }

    #[test]
    fn point_zero_dest_rect_returns_none() {
        let c = cfg((0.0, 0.0, 1920.0, 1080.0), (0.0, 0.0, 0.0, 0.0), 0);
        assert!(display_point_to_texture(0.0, 0.0, &c).is_none());
    }
}
