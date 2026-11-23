//! VS Code's image preview (the built-in Media Preview extension): images
//! open in a viewer instead of the text editor, fit to the editor until
//! zoomed, with their size and the zoom in the status bar.

use std::{cell::Cell, fs, path::Path, rc::Rc};

use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;

/// The extensions VS Code's image preview opens.
const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "gif", "webp", "bmp", "ico", "tif", "tiff", "svg", "pbm",
    "pgm", "ppm",
];

/// VS Code's zoom levels (media preview `zoomLevels`).
const LEVELS: &[f32] = &[
    0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1., 1.5, 2., 3., 5., 7., 10., 15., 20.,
];

pub(super) fn is_image(path: &Path) -> bool {
    (path.extension().and_then(|e| e.to_str()))
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub(super) struct ImageView {
    /// Width and height in pixels; None when they can't be read.
    pub(super) size: Option<(u32, u32)>,
    pub(super) bytes: u64,
    /// None: the whole image, scaled down to fit ("Whole Image").
    pub(super) zoom: Option<f32>,
    /// The viewer's size as last drawn, for the scale a fit image is at.
    viewport: Rc<Cell<Size<Pixels>>>,
    scroll: ScrollHandle,
}

impl ImageView {
    pub(super) fn open(path: &Path) -> Self {
        let bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Self {
            size: dimensions(path),
            bytes,
            zoom: None,
            viewport: Rc::default(),
            scroll: ScrollHandle::new(),
        }
    }

    /// The scale the image is drawn at now.
    fn scale(&self) -> f32 {
        if let Some(z) = self.zoom {
            return z;
        }
        let (Some((w, h)), view) = (self.size, self.viewport.get()) else {
            return 1.;
        };
        let fit = (f32::from(view.width) / w as f32).min(f32::from(view.height) / h as f32);
        if fit.is_finite() && fit > 0. { fit.min(1.) } else { 1. }
    }

    /// One level in (`dir` 1) or out (-1) from the current scale.
    pub(super) fn step(&mut self, dir: i32) {
        if self.size.is_none() {
            return;
        }
        let now = self.scale();
        let next = if dir > 0 {
            LEVELS.iter().copied().find(|&l| l > now + 1e-3)
        } else {
            LEVELS.iter().rev().copied().find(|&l| l < now - 1e-3)
        };
        if let Some(z) = next {
            self.zoom = Some(z);
        }
    }

    /// The status bar's zoom entry, as VS Code labels it.
    pub(super) fn zoom_label(&self) -> String {
        match self.zoom {
            None => "Whole Image".into(),
            Some(z) => format!("{}%", (z * 100.).round()),
        }
    }

    pub(super) fn size_label(&self) -> Option<String> {
        self.size.map(|(w, h)| format!("{w}x{h}"))
    }

    pub(super) fn bytes_label(&self) -> String {
        let b = self.bytes as f64;
        match self.bytes {
            0..1024 => format!("{}B", self.bytes),
            1024..1_048_576 => format!("{:.2}KB", b / 1024.),
            1_048_576..1_073_741_824 => format!("{:.2}MB", b / 1_048_576.),
            _ => format!("{:.2}GB", b / 1_073_741_824.),
        }
    }

    /// The viewer: the image centered on the editor background, scaled down
    /// to fit until zoomed, then at its zoom in a scrolling view.
    pub(super) fn render(&self, path: &Path, id: usize) -> Stateful<Div> {
        let viewport = self.viewport.clone();
        let image = img(path.to_path_buf()).flex_none();
        let image = match (self.zoom, self.size) {
            (Some(z), Some((w, h))) => image.w(px(w as f32 * z)).h(px(h as f32 * z)),
            // Whole Image: at most its own size, as VS Code's scale-down.
            _ => image
                .max_w_full()
                .max_h_full()
                .object_fit(ObjectFit::ScaleDown)
                .when_some(self.size, |i, (w, h)| i.w(px(w as f32)).h(px(h as f32))),
        };
        div()
            .id(("image-preview", id))
            .relative()
            .size_full()
            .overflow_scroll()
            // ponytail: gpui has no zoom-in/out cursors, VS Code's here.
            .track_scroll(&self.scroll)
            .child(
                canvas(
                    move |bounds, _, _| viewport.set(bounds.size),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .min_w_full()
                    .min_h_full()
                    .when(self.zoom.is_none(), |d| d.size_full())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(image),
            )
    }
}

/// An image's width and height from its header, or an SVG's attributes.
fn dimensions(path: &Path) -> Option<(u32, u32)> {
    if let Ok(s) = imagesize::size(path) {
        return Some((s.width as u32, s.height as u32));
    }
    let svg = fs::read_to_string(path).ok()?;
    let start = svg.find("<svg")?;
    let tag = &svg[start..start + svg[start..].find('>')?];
    let attr = |name: &str| {
        let at = tag.find(&format!(" {name}="))? + name.len() + 2;
        let quote = tag[at..].chars().next()?;
        let value = &tag[at + 1..];
        Some(&value[..value.find(quote)?])
    };
    let number = |v: &str| {
        let v = v.trim().trim_end_matches("px");
        v.parse::<f32>().ok().filter(|n| *n > 0.)
    };
    let (w, h) = match (attr("width").and_then(number), attr("height").and_then(number)) {
        (Some(w), Some(h)) => (w, h),
        _ => {
            let vb: Vec<f32> = (attr("viewBox")?.split([' ', ','].as_slice()))
                .filter_map(|n| n.parse().ok())
                .collect();
            (*vb.get(2)?, *vb.get(3)?)
        }
    };
    Some((w.round() as u32, h.round() as u32))
}

#[cfg(test)]
mod tests {
    // Not super::*: gpui_kit's glob brings gpui's #[test], which shadows std's.
    use super::{Cell, ImageView, Rc, ScrollHandle, dimensions, fs};
    use gpui_kit::{px, size};

    #[test]
    fn zoom_steps_through_vs_codes_levels() {
        let mut v = ImageView {
            size: Some((100, 100)),
            bytes: 2048,
            zoom: None,
            viewport: Rc::new(Cell::new(size(px(50.), px(80.)))),
            scroll: ScrollHandle::new(),
        };
        // Fit is 50%: in goes to 60%, out from there to 50%.
        v.step(1);
        assert_eq!(v.zoom, Some(0.6));
        v.step(-1);
        assert_eq!(v.zoom_label(), "50%");
        assert_eq!(v.bytes_label(), "2.00KB");
        assert_eq!(v.size_label().as_deref(), Some("100x100"));
    }

    #[test]
    fn svg_size_from_attributes_or_view_box() {
        let dir = std::env::temp_dir();
        let a = dir.join("vsn-test-a.svg");
        fs::write(&a, r#"<svg xmlns="x" width="24px" height="16">"#).unwrap();
        assert_eq!(dimensions(&a), Some((24, 16)));
        fs::write(&a, r#"<svg viewBox="0 0 48 32" xmlns="x">"#).unwrap();
        assert_eq!(dimensions(&a), Some((48, 32)));
        fs::remove_file(a).ok();
    }
}
