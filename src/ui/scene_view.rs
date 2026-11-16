//! Rendering helpers shared by the scene view and the prompter view: colour
//! conversion, content geometry (fit/fill/stretch + rotation) and Pango layout
//! construction. All math here is pure and unit-tested.

use crate::model::layer::{Rgba, TextAlign, TextStyle};
use crate::model::project::OutputScale;
use crate::ui::render::ViewMode;
use gtk::gdk;
use gtk::pango;
use gtk4 as gtk;

/// Convert a model colour into a GDK colour.
pub fn gdk_rgba(color: Rgba) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        f32::from(color.a) / 255.0,
    )
}

/// Parse a `#RRGGBB` string straight into a GDK colour (invalid -> white).
pub fn gdk_rgba_str(value: &str) -> gdk::RGBA {
    gdk_rgba(Rgba::parse(value))
}

/// Where the design canvas lands inside a viewport, and the scale factors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentGeometry {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub sx: f64,
    pub sy: f64,
    pub rotation: u16,
}

impl ContentGeometry {
    /// Design point -> view point (before the rotation transform).
    pub fn to_view(&self, dx: f64, dy: f64) -> (f64, f64) {
        (self.x + dx * self.sx, self.y + dy * self.sy)
    }

    /// View point -> design point (before the rotation transform).
    pub fn to_design(&self, vx: f64, vy: f64) -> (f64, f64) {
        let sx = if self.sx.abs() < f64::EPSILON {
            1.0
        } else {
            self.sx
        };
        let sy = if self.sy.abs() < f64::EPSILON {
            1.0
        } else {
            self.sy
        };
        ((vx - self.x) / sx, (vy - self.y) / sy)
    }

    /// Whether the geometry depends on rotation at all.
    pub fn rotated(&self) -> bool {
        self.rotation % 360 != 0
    }
}

/// Compute the content placement for a viewport.
///
/// `zoom` is only used by the editor: `0.0` means "fit", any positive value is
/// design pixels to screen pixels (1.0 = 100 %) anchored top-left so a scrolled
/// window can pan it.
#[allow(clippy::too_many_arguments)]
pub fn content_geometry(
    viewport: (f64, f64),
    design: (f64, f64),
    scale: OutputScale,
    rotation: u16,
    zoom: f64,
    mode: ViewMode,
) -> ContentGeometry {
    let (vw, vh) = (viewport.0.max(1.0), viewport.1.max(1.0));
    let (dw, dh) = (design.0.max(1.0), design.1.max(1.0));
    let rotation = if mode == ViewMode::Editor {
        0
    } else {
        rotation % 360
    };

    if mode == ViewMode::Editor && zoom > 0.0 {
        return ContentGeometry {
            x: 0.0,
            y: 0.0,
            w: dw * zoom,
            h: dh * zoom,
            sx: zoom,
            sy: zoom,
            rotation: 0,
        };
    }

    // Rotated output: the rotated design must fit, so swap the fitted dims.
    let (ew, eh) = if rotation == 90 || rotation == 270 {
        (dh, dw)
    } else {
        (dw, dh)
    };

    let (sx, sy) = match scale {
        OutputScale::Fit => {
            let s = (vw / ew).min(vh / eh);
            (s, s)
        }
        OutputScale::Fill => {
            let s = (vw / ew).max(vh / eh);
            (s, s)
        }
        OutputScale::Stretch => (vw / ew, vh / eh),
    };

    let w = dw * sx;
    let h = dh * sy;
    ContentGeometry {
        x: (vw - w) / 2.0,
        y: (vh - h) / 2.0,
        w,
        h,
        sx,
        sy,
        rotation,
    }
}

/// Inner rect for a media layer inside its layer box.
pub fn fit_rect(
    (bx, by, bw, bh): (f64, f64, f64, f64),
    source: (f64, f64),
    scale: OutputScale,
) -> (f64, f64, f64, f64) {
    let (sw, sh) = (source.0.max(1.0), source.1.max(1.0));
    match scale {
        OutputScale::Stretch => (bx, by, bw, bh),
        OutputScale::Fit | OutputScale::Fill => {
            let s = match scale {
                OutputScale::Fit => (bw / sw).min(bh / sh),
                _ => (bw / sw).max(bh / sh),
            };
            let w = sw * s;
            let h = sh * s;
            (bx + (bw - w) / 2.0, by + (bh - h) / 2.0, w, h)
        }
    }
}

fn pango_alignment(align: TextAlign) -> pango::Alignment {
    match align {
        TextAlign::Left => pango::Alignment::Left,
        TextAlign::Center => pango::Alignment::Center,
        TextAlign::Right => pango::Alignment::Right,
    }
}

/// Build a Pango layout for a style at the given width, scaled for the viewport.
pub fn text_layout(
    context: &pango::Context,
    text: &str,
    style: &TextStyle,
    width_px: f64,
    scale: f64,
) -> pango::Layout {
    let layout = pango::Layout::new(context);
    let mut description = pango::FontDescription::new();
    description.set_family(&style.family);
    let size = (style.size * scale).max(1.0);
    description.set_absolute_size(size * f64::from(pango::SCALE));
    description.set_weight(if style.bold {
        pango::Weight::Bold
    } else {
        pango::Weight::Normal
    });
    description.set_style(if style.italic {
        pango::Style::Italic
    } else {
        pango::Style::Normal
    });
    layout.set_font_description(Some(&description));
    let shown = if style.uppercase {
        text.to_uppercase()
    } else {
        text.to_string()
    };
    layout.set_text(&shown);
    layout.set_width((width_px.max(1.0) * f64::from(pango::SCALE)) as i32);
    layout.set_wrap(pango::WrapMode::WordChar);
    layout.set_alignment(pango_alignment(style.align));
    // `set_line_spacing` is not the multiplier the model uses (1.0 = normal), so
    // convert the multiplier into absolute extra spacing between lines.
    let extra = ((style.line_spacing - 1.0).max(0.0) * size * 1.2) * f64::from(pango::SCALE);
    layout.set_spacing(extra.round() as i32);
    layout
}

/// Layout size in pixels.
pub fn layout_size(layout: &pango::Layout) -> (f64, f64) {
    let (w, h) = layout.size();
    (
        w as f64 / f64::from(pango::SCALE),
        h as f64 / f64::from(pango::SCALE),
    )
}

/// Rect covering the full widget, used for background fills and clips.
pub fn full_rect(w: f64, h: f64) -> gtk::graphene::Rect {
    gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32)
}

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> gtk::graphene::Rect {
    gtk::graphene::Rect::new(x as f32, y as f32, w.max(0.0) as f32, h.max(0.0) as f32)
}

/// Width of a laid-out string in pixels, for ticker repetition math.
pub fn text_extent(context: &pango::Context, text: &str, style: &TextStyle, scale: f64) -> f64 {
    let layout = text_layout(context, text, style, 100_000.0, scale);
    layout_size(&layout).0
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: (f64, f64) = (1280.0, 720.0);
    const DESIGN: (f64, f64) = (1920.0, 1080.0);

    #[test]
    fn fit_letterboxes_and_centres() {
        let geo = content_geometry(VIEW, DESIGN, OutputScale::Fit, 0, 0.0, ViewMode::Program);
        assert!((geo.x - 0.0).abs() < 0.001);
        assert!((geo.w - 1280.0).abs() < 0.001);
        assert!((geo.h - 720.0).abs() < 0.001);

        // Portrait viewport: pillarboxed.
        let geo = content_geometry(
            (600.0, 800.0),
            DESIGN,
            OutputScale::Fit,
            0,
            0.0,
            ViewMode::Program,
        );
        assert!((geo.w - 600.0).abs() < 0.001);
        assert!((geo.h - 337.5).abs() < 0.001);
        assert!((geo.y - 231.25).abs() < 0.001);
    }

    #[test]
    fn fill_crops_and_stretch_distorts() {
        let geo = content_geometry(
            (600.0, 800.0),
            DESIGN,
            OutputScale::Fill,
            0,
            0.0,
            ViewMode::Program,
        );
        assert!(geo.h >= 800.0);
        assert!(geo.w > 600.0);

        let geo = content_geometry(
            (600.0, 800.0),
            DESIGN,
            OutputScale::Stretch,
            0,
            0.0,
            ViewMode::Program,
        );
        assert!((geo.w - 600.0).abs() < 0.001);
        assert!((geo.h - 800.0).abs() < 0.001);
        assert!((geo.sx - geo.sy).abs() > 0.01);
    }

    #[test]
    fn rotation_swaps_the_fitted_dimensions() {
        let geo = content_geometry(VIEW, DESIGN, OutputScale::Fit, 90, 0.0, ViewMode::Program);
        // Rotated 90°, the 16:9 design fills the 16:9 viewport's height, so the
        // pre-rotation box is portrait sized: displayed size = 405x720.
        assert!((geo.h - 405.0).abs() < 0.001, "h={}", geo.h);
        assert!((geo.w - 720.0).abs() < 0.001, "w={}", geo.w);
        assert_eq!(geo.rotation, 90);

        let portrait_view = (600.0, 800.0);
        let geo = content_geometry(
            portrait_view,
            DESIGN,
            OutputScale::Fit,
            90,
            0.0,
            ViewMode::Program,
        );
        assert!((geo.h - 450.0).abs() < 0.001, "h={}", geo.h);
        assert!((geo.w - 800.0).abs() < 0.001, "w={}", geo.w);
    }

    #[test]
    fn editor_zoom_is_top_left_and_rotation_free() {
        let geo = content_geometry(VIEW, DESIGN, OutputScale::Fit, 90, 0.5, ViewMode::Editor);
        assert_eq!(geo.x, 0.0);
        assert_eq!(geo.y, 0.0);
        assert_eq!(geo.w, 960.0);
        assert_eq!(geo.sx, 0.5);
        assert_eq!(geo.rotation, 0);

        let fit = content_geometry(VIEW, DESIGN, OutputScale::Fit, 0, 0.0, ViewMode::Editor);
        assert!((fit.w - 1280.0).abs() < 0.001);
        assert!((fit.y - 0.0).abs() < 0.001);
    }

    #[test]
    fn view_and_design_coordinates_round_trip() {
        let geo = content_geometry(VIEW, DESIGN, OutputScale::Fit, 0, 0.0, ViewMode::Program);
        let (vx, vy) = geo.to_view(960.0, 540.0);
        assert!((vx - 640.0).abs() < 0.001);
        assert!((vy - 360.0).abs() < 0.001);
        let (dx, dy) = geo.to_design(vx, vy);
        assert!((dx - 960.0).abs() < 0.001);
        assert!((dy - 540.0).abs() < 0.001);
    }

    #[test]
    fn media_fit_rect_keeps_aspect() {
        let (x, y, w, h) = fit_rect((0.0, 0.0, 100.0, 100.0), (16.0, 9.0), OutputScale::Fit);
        assert!((w - 100.0).abs() < 0.001);
        assert!((h - 56.25).abs() < 0.001);
        assert!((y - 21.875).abs() < 0.001);
        assert_eq!(x, 0.0);

        let (_, _, w, h) = fit_rect((0.0, 0.0, 100.0, 100.0), (16.0, 9.0), OutputScale::Stretch);
        assert_eq!((w, h), (100.0, 100.0));

        let (_, _, w, h) = fit_rect((10.0, 10.0, 100.0, 100.0), (16.0, 9.0), OutputScale::Fill);
        assert!(w >= 100.0);
        assert!(h > 56.25);
    }
}
