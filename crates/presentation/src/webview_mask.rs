//! Letting the window draw over a page.
//!
//! A page is a native view put over the window, so everything the window itself draws in the same
//! place (the palette, a menu, a toast) would be hidden behind it, and would not get the mouse.
//! Where the system can make a picture of the page, the page is hidden while something is over it
//! and its picture is drawn by the window in its place. Elsewhere the page is only hidden.

use gpui::{Bounds, Pixels};

/// A rectangle in window coordinates: left, top, width, height.
pub(crate) type Rect = (f32, f32, f32, f32);

pub(crate) fn rect_of(bounds: Bounds<Pixels>) -> Rect {
    (f32::from(bounds.origin.x), f32::from(bounds.origin.y), f32::from(bounds.size.width), f32::from(bounds.size.height))
}

fn overlap(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

fn union(a: Rect, b: Rect) -> Rect {
    let (left, top) = (a.0.min(b.0), a.1.min(b.1));
    let (right, bottom) = ((a.0 + a.2).max(b.0 + b.2), (a.1 + a.3).max(b.1 + b.3));
    (left, top, right - left, bottom - top)
}

/// The parts of `overlays` that fall on `page`, as holes that do not overlap each other (holes
/// that touch are joined into one, since a hole inside a hole would fill it again).
pub(crate) fn holes_in(page: Rect, overlays: &[Rect]) -> Vec<Rect> {
    let mut holes: Vec<Rect> = Vec::new();
    for overlay in overlays {
        if !overlap(page, *overlay) {
            continue;
        }
        let left = overlay.0.max(page.0);
        let top = overlay.1.max(page.1);
        let right = (overlay.0 + overlay.2).min(page.0 + page.2);
        let bottom = (overlay.1 + overlay.3).min(page.1 + page.3);
        let mut hole = (left, top, right - left, bottom - top);
        // Join with every hole it meets, until none is left to meet.
        loop {
            match holes.iter().position(|other| overlap(hole, *other)) {
                Some(at) => hole = union(hole, holes.remove(at)),
                None => break,
            }
        }
        holes.push(hole);
    }
    holes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    holes
}

#[cfg(target_os = "macos")]
pub(crate) use mac::NativeView;

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::{AnyThread, ClassType, MainThreadMarker};
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSView};
    use objc2_foundation::{NSDictionary, NSError, NSNumber, NSObjectProtocol};
    use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// The view of a page, as the window holds it.
    pub(crate) struct NativeView {
        webview: Retained<WKWebView>,
        /// The picture of the page, as PNG, when the system has made it.
        shot: Arc<Mutex<Option<Vec<u8>>>>,
    }

    /// An image as PNG bytes.
    fn png_of(image: &NSImage) -> Option<Vec<u8>> {
        let tiff = image.TIFFRepresentation()?;
        let rep = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)?;
        // SAFETY: no properties are given.
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
        Some(data.to_vec())
    }

    impl NativeView {
        /// The newest subview of the window's content view: the one `wry` just added.
        pub(crate) fn newest_in(window: &impl HasWindowHandle) -> Option<Self> {
            let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
            // SAFETY: the handle is the window's content view, which lives as long as the window.
            let parent: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
            let views = parent.subviews();
            let view = views.lastObject()?;
            if !view.isKindOfClass(WKWebView::class()) {
                return None;
            }
            // SAFETY: checked to be a web view just above.
            let webview = unsafe { Retained::cast_unchecked::<WKWebView>(view) };
            Some(Self { webview, shot: Arc::new(Mutex::new(None)) })
        }

        /// Ask the system for a picture of the page, `width` points wide at twice the resolution.
        /// It comes later; `take_snapshot` has it.
        pub(crate) fn request_snapshot(&self, width: f64) {
            let Some(thread) = MainThreadMarker::new() else { return };
            // SAFETY: a plain configuration object.
            let config = unsafe { WKSnapshotConfiguration::new(thread) };
            // SAFETY: setting the width of the picture.
            unsafe { config.setSnapshotWidth(Some(&NSNumber::new_f64(width * 2.0))) };
            let slot = self.shot.clone();
            let handler = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
                // SAFETY: the system passes an image or null.
                if let Some(image) = unsafe { image.as_ref() } {
                    if let Ok(mut slot) = slot.lock() {
                        *slot = png_of(image);
                    }
                }
            });
            // SAFETY: the handler lives as long as the system needs it (it is copied).
            unsafe { self.webview.takeSnapshotWithConfiguration_completionHandler(Some(&config), &handler) };
        }

        /// The picture the system made, once.
        pub(crate) fn take_snapshot(&self) -> Option<Vec<u8>> {
            self.shot.lock().ok()?.take()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_part_of_an_overlay_on_the_page_is_cut_out() {
        let page = (100.0, 100.0, 400.0, 300.0);
        // One inside, one half outside, one away.
        let overlays = [(150.0, 120.0, 100.0, 50.0), (450.0, 350.0, 200.0, 200.0), (0.0, 0.0, 50.0, 50.0)];
        assert_eq!(holes_in(page, &overlays), vec![(150.0, 120.0, 100.0, 50.0), (450.0, 350.0, 50.0, 50.0)]);
        assert!(holes_in(page, &[(0.0, 0.0, 50.0, 50.0)]).is_empty());
    }

    #[test]
    fn overlays_that_meet_become_one_hole_so_none_fills_another_again() {
        let page = (0.0, 0.0, 500.0, 500.0);
        let holes = holes_in(page, &[(10.0, 10.0, 100.0, 100.0), (60.0, 60.0, 100.0, 100.0), (300.0, 300.0, 10.0, 10.0), (150.0, 10.0, 20.0, 200.0)]);
        // The first two meet, and the fourth meets their union.
        assert_eq!(holes, vec![(10.0, 10.0, 160.0, 200.0), (300.0, 300.0, 10.0, 10.0)]);
    }
}
