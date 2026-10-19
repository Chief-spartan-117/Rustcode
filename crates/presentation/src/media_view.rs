//! Files that are not text: pictures show in a viewer, audio and video in a card that opens them
//! in the system player. SVG files stay text, with a live preview like Markdown has.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use gpui::{div, img, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, ObjectFit, RenderImage};
use rustcode_domain::DocumentId;

use crate::editor_view::EditorView;
use crate::text_layout::UI_FONT_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaKind {
    Image,
    Video,
    Audio,
}

const IMAGES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff"];
const VIDEOS: &[&str] = &["mp4", "m4v", "mov", "webm", "mkv", "avi", "wmv", "flv", "mpg", "mpeg", "ogv", "3gp"];
const AUDIO: &[&str] = &["mp3", "wav", "flac", "ogg", "oga", "m4a", "aac", "opus", "wma", "aiff"];

/// What kind of non-text file `path` is, by its extension.
pub(crate) fn media_kind(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let ext = ext.as_str();
    if IMAGES.contains(&ext) {
        Some(MediaKind::Image)
    } else if VIDEOS.contains(&ext) {
        Some(MediaKind::Video)
    } else if AUDIO.contains(&ext) {
        Some(MediaKind::Audio)
    } else {
        None
    }
}

/// Whether the file called `title` is an SVG picture, which opens as text.
pub(crate) fn is_svg(title: &str) -> bool {
    title.to_ascii_lowercase().ends_with(".svg")
}

/// Whether a file can be shown rendered beside or instead of its text.
pub(crate) fn is_previewable(title: &str) -> bool {
    crate::markdown_view::is_markdown(title) || is_svg(title)
}

/// Width and height of a PNG, GIF, BMP or JPEG picture, read from its first bytes.
pub(crate) fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let le16 = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u32);
    let le32 = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((be32(16)?, be32(20)?));
    }
    if bytes.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if bytes.starts_with(b"BM") {
        return Some((le32(18)?, (le32(22)? as i32).unsigned_abs()));
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        // Walk the segments to the one that starts a frame.
        let mut at = 2;
        while at + 9 < bytes.len() {
            if bytes[at] != 0xff {
                at += 1;
                continue;
            }
            let marker = bytes[at + 1];
            if marker == 0xff {
                at += 1;
                continue;
            }
            let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                let height = u16::from_be_bytes([bytes[at + 5], bytes[at + 6]]) as u32;
                let width = u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]) as u32;
                return Some((width, height));
            }
            at += 2 + length;
        }
    }
    None
}

pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// `file://` address of `path`, for handing to the system.
pub(crate) fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => url.push(byte as char),
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// System fonts for text inside SVG files, read once.
fn svg_fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTS: std::sync::LazyLock<Arc<resvg::usvg::fontdb::Database>> = std::sync::LazyLock::new(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });
    FONTS.clone()
}

/// Draw SVG source as a picture at `scale` times its own size, in the byte order the window wants
/// (blue first, with the color already scaled by transparency). `None` when it is not a valid SVG.
pub(crate) fn rasterize_svg(source: &[u8], scale: f32) -> Option<Arc<RenderImage>> {
    let options = resvg::usvg::Options { fontdb: svg_fonts(), ..Default::default() };
    let tree = resvg::usvg::Tree::from_data(source, &options).ok()?;
    let size = tree.size();
    let (width, height) = ((size.width() * scale).ceil() as u32, (size.height() * scale).ceil() as u32);
    // Bigger than any screen is not a picture worth drawing.
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return None;
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let mut pixels = pixmap.take();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = image::ImageBuffer::from_raw(width, height, pixels)?;
    Some(Arc::new(RenderImage::new(smallvec::SmallVec::from_elem(image::Frame::new(buffer), 1))))
}

impl EditorView {
    /// Whether document `id` is a file this editor shows but cannot edit.
    pub(crate) fn is_media_doc(&self, id: DocumentId) -> bool {
        self.doc_path(id).and_then(media_kind).is_some()
    }

    /// Open a picture, sound or video: a tab with no text behind it.
    pub(crate) fn open_media(&mut self, path: &Path, cx: &mut Context<Self>) {
        let open = self.docs.iter().map(|slot| slot.id).find(|id| self.doc_path(*id) == Some(path));
        let id = match open {
            Some(id) => id,
            None => {
                if !path.is_file() {
                    self.set_status(format!("cannot find {}", path.display()));
                    cx.notify();
                    return;
                }
                let id = self.allocate_document_id();
                let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let document = rustcode_domain::Document::new(id, title, rustcode_domain::TextBuffer::default()).with_path(path.to_path_buf());
                self.log(format!("opened {}", path.display()));
                self.add_document(document)
            }
        };
        self.open_doc_in_active_group(id, cx);
    }

    /// The picture viewer, or the card for sound and video, of group `gi`.
    pub(crate) fn render_media(&self, gi: usize, cx: &mut Context<Self>) -> AnyElement {
        let group = &self.groups[gi];
        let doc = group.tabs[group.active].doc;
        let theme = &self.theme;
        let Some(path) = self.doc_path(doc).map(Path::to_path_buf) else { return div().into_any_element() };
        let kind = media_kind(&path).unwrap_or(MediaKind::Image);
        let size = std::fs::metadata(&path).map(|m| human_size(m.len())).unwrap_or_default();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (text, dim, border, hover) = (theme.text, theme.gutter_text, theme.border, theme.lists.hover);
        let button = |id: &'static str, label: &'static str| {
            div()
                .id((id, group.id.0 as usize))
                .px(px(10.))
                .h(px(24.))
                .flex()
                .items_center()
                .rounded(px(3.))
                .border_1()
                .border_color(border)
                .text_color(text)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .child(label)
        };
        let frame = div()
            .id(("media", group.id.0 as usize))
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(text)
            .text_size(px(UI_FONT_SIZE));
        match kind {
            MediaKind::Image => {
                let actual = self.media_actual.contains(&doc);
                let dimensions = {
                    let mut head = Vec::new();
                    std::fs::File::open(&path).ok().and_then(|f| f.take(256 * 1024).read_to_end(&mut head).ok());
                    image_size(&head).map(|(w, h)| format!("{w} \u{00d7} {h}  \u{00b7}  ")).unwrap_or_default()
                };
                let picture = img(path.clone()).with_fallback(move || {
                    div().p(px(24.)).text_color(dim).child("This picture cannot be shown.").into_any_element()
                });
                frame
                    .child(
                        div()
                            .h(px(34.))
                            .flex_none()
                            .px(px(12.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(12.))
                            .border_b_1()
                            .border_color(border)
                            .child(div().text_color(dim).child(format!("{dimensions}{size}")))
                            .child(
                                button("media-zoom", if actual { "Fit to window" } else { "Actual size" }).on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                        if !this.media_actual.remove(&doc) {
                                            this.media_actual.insert(doc);
                                        }
                                        cx.notify();
                                    }),
                                ),
                            ),
                    )
                    .child(if actual {
                        div()
                            .id(("media-scroll", group.id.0 as usize))
                            .flex_1()
                            .min_h_0()
                            .overflow_scroll()
                            .p(px(16.))
                            .child(picture)
                            .into_any_element()
                    } else {
                        div()
                            .flex_1()
                            .min_h_0()
                            .p(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(picture.max_w_full().max_h_full().object_fit(ObjectFit::Contain))
                            .into_any_element()
                    })
                    .into_any_element()
            }
            MediaKind::Video | MediaKind::Audio => {
                let what = if kind == MediaKind::Video { "Video" } else { "Audio" };
                let url = file_url(&path);
                frame
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(10.))
                            .child(div().text_size(px(UI_FONT_SIZE + 4.)).child(name))
                            .child(div().text_color(dim).child(format!("{what}  \u{00b7}  {size}")))
                            .child(
                                button("media-open", "Open in Default Player").on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |_this, _: &MouseDownEvent, _w, cx| cx.open_url(&url)),
                                ),
                            )
                            .child(div().text_color(dim).text_size(px(UI_FONT_SIZE - 1.)).child("RustCode does not play sound or video itself.")),
                    )
                    .into_any_element()
            }
        }
    }

    /// The SVG of group `gi` drawn as a picture, from the text as it is now.
    pub(crate) fn render_svg_preview(&self, gi: usize, scale: f32) -> AnyElement {
        let group = &self.groups[gi];
        let doc = group.tabs[group.active].doc;
        let theme = &self.theme;
        let (document, _) = self.doc_ref(doc);
        let revision = document.editor.revision();
        let image = {
            let mut cache = self.svg_cache.borrow_mut();
            match cache.as_ref() {
                Some((id, rev, image)) if *id == doc && *rev == revision => image.clone(),
                _ => {
                    let image = rasterize_svg(document.editor.buffer().text().as_bytes(), scale);
                    *cache = Some((doc, revision, image.clone()));
                    image
                }
            }
        };
        let dim = theme.gutter_text;
        div()
            .id(("svg-preview", group.id.0 as usize))
            .flex_1()
            .min_w_0()
            .h_full()
            .p(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border)
            .child(match image {
                Some(image) => img(image).max_w_full().max_h_full().object_fit(ObjectFit::Contain).into_any_element(),
                None => div().text_color(dim).child("This SVG cannot be drawn.").into_any_element(),
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_come_from_the_extension() {
        assert_eq!(media_kind(Path::new("/a/b.PNG")), Some(MediaKind::Image));
        assert_eq!(media_kind(Path::new("clip.mp4")), Some(MediaKind::Video));
        assert_eq!(media_kind(Path::new("song.flac")), Some(MediaKind::Audio));
        assert_eq!(media_kind(Path::new("logo.svg")), None);
        assert_eq!(media_kind(Path::new("notes.md")), None);
        assert!(is_previewable("logo.svg") && is_previewable("README.md") && !is_previewable("a.rs"));
    }

    #[test]
    fn sizes_are_read_from_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend([0, 0, 1, 0, 0, 0, 0, 0x80]);
        assert_eq!(image_size(&png), Some((256, 128)));
        let mut gif = b"GIF89a".to_vec();
        gif.extend([10, 0, 20, 0]);
        assert_eq!(image_size(&gif), Some((10, 20)));
        // A JPEG with one filler segment, then a frame header of 30 x 40.
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 11, 8, 0, 40, 0, 30, 1, 0, 0];
        assert_eq!(image_size(&jpeg), Some((30, 40)));
        assert_eq!(image_size(b"not a picture"), None);
    }

    #[test]
    fn svg_is_drawn_blue_first_and_bad_svg_is_refused() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="#ff0000"/></svg>"##;
        let image = rasterize_svg(svg, 2.0).unwrap();
        // Red in blue-green-red-alpha order.
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
        assert_eq!(image.size(0).width.0, 4);
        assert!(rasterize_svg(b"<svg", 1.0).is_none());
    }

    #[test]
    fn sizes_and_addresses_are_readable() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(file_url(Path::new("/a b/\u{e9}.mp4")), "file:///a%20b/%C3%A9.mp4");
    }
}
