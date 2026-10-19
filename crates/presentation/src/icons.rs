//! Interface icons: small single-color shapes tinted with a theme color.

use gpui::{prelude::*, px, svg, Rgba, Svg};

/// The icon `name` (a file under `ui/` in the bundled resources), `size` pixels square.
pub(crate) fn icon(name: &str, size: f32, color: Rgba) -> Svg {
    svg().path(format!("ui/{name}.svg")).w(px(size)).h(px(size)).flex_none().text_color(color)
}
