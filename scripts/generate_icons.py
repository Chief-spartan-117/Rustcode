#!/usr/bin/env python3
"""Generates the bundled interface icons (Phosphor), the application icon, and the Rust file that
embeds every bundled resource.

Run from the repository root:  python3 scripts/generate_icons.py
Interface icons are single-color Phosphor icons that the view tints with the theme's text color.
File and folder icons are the Material Icon Theme, imported by scripts/material_icons.py.
"""
import json
import os

ASSETS = "apps/rustcode/assets"
ICONS = f"{ASSETS}/icons"
UI = f"{ASSETS}/ui"


def svg(body, size=16):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" width="{size}" height="{size}">'
        f"{body}</svg>\n"
    )


def stroke(d, color="#ffffff", width=1.5):
    return (
        f'<path d="{d}" fill="none" stroke="{color}" stroke-width="{width}" '
        f'stroke-linecap="round" stroke-linejoin="round"/>'
    )


def badge(color, glyph):
    """Rounded square with a glyph, readable down to 14 px."""
    return svg(f'<rect x="1" y="1" width="14" height="14" rx="3" fill="{color}"/>{glyph}')


# ---- Phosphor icons ---------------------------------------------------------------------------
# https://phosphoricons.com (MIT). The files used are kept in scripts/phosphor/<weight>/.

PHOSPHOR = "scripts/phosphor"


def phosphor(name, weight="regular", color="#000000"):
    """One Phosphor icon in `color`. Interface icons stay black: the view tints them."""
    suffix = "" if weight == "regular" else f"-{weight}"
    with open(f"{PHOSPHOR}/{weight}/{name}{suffix}.svg") as f:
        text = f.read().strip()
    return text.replace('fill="currentColor"', f'fill="{color}"') + "\n"


# ---- interface icons ------------------------------------------------------------------------
# name: (phosphor icon, weight). Small icons use the bold weight so they hold at 10 to 12 px.

UI_ICONS = {
    "explorer": ("files", "regular"),
    "search": ("magnifying-glass", "regular"),
    "git": ("git-fork", "regular"),
    "panel": ("square-half-bottom", "regular"),
    "preview": ("columns", "regular"),
    "preview-panel": ("browsers", "regular"),
    "preview-page": ("file-text", "regular"),
    "checkbox": ("square", "regular"),
    "checkbox-checked": ("check-square", "fill"),
    "diff": ("git-diff", "regular"),
    "diff-split": ("square-split-horizontal", "regular"),
    "arrow-down": ("arrow-down", "bold"),
    "arrow-up": ("arrow-up", "bold"),
    "refresh": ("arrow-clockwise", "bold"),
    "discard": ("arrow-counter-clockwise", "bold"),
    "close": ("x", "bold"),
    "dot": ("circle", "fill"),
    "chevron-right": ("caret-right", "bold"),
    "chevron-down": ("caret-down", "bold"),
    "chevron-up": ("caret-up", "bold"),
    "plus": ("plus", "bold"),
    "minus": ("minus", "bold"),
    "error": ("x-circle", "fill"),
    "warning": ("warning", "fill"),
    "info": ("info", "fill"),
    "replace": ("swap", "bold"),
    "replace-all": ("arrows-left-right", "bold"),
    "minimize": ("minus", "bold"),
    "maximize": ("corners-out", "regular"),
    "branch": ("git-branch", "bold"),
    "stash": ("archive", "regular"),
    "trash": ("trash", "regular"),
    # The debugger.
    "debug-continue": ("play", "fill"),
    "debug-start": ("play", "fill"),
    "debug-pause": ("pause", "fill"),
    "debug-step-over": ("arrow-arc-right", "bold"),
    "debug-step-into": ("arrow-line-down", "bold"),
    "debug-step-out": ("arrow-line-up", "bold"),
    "debug-restart": ("arrow-counter-clockwise", "bold"),
    "debug-stop": ("stop", "fill"),
    "debug-grip": ("dots-six-vertical", "bold"),
}

ui = {name: phosphor(icon, weight) for name, (icon, weight) in UI_ICONS.items()}

# ---- application icon -----------------------------------------------------------------------

app_icon = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#2b3140"/>
      <stop offset="1" stop-color="#12151d"/>
    </linearGradient>
    <linearGradient id="brace" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#ffb066"/>
      <stop offset="1" stop-color="#e2582d"/>
    </linearGradient>
    <linearGradient id="caret" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#5eead4"/>
      <stop offset="1" stop-color="#22a6b3"/>
    </linearGradient>
    <linearGradient id="shine" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.16"/>
      <stop offset="0.5" stop-color="#ffffff" stop-opacity="0"/>
    </linearGradient>
  </defs>
  <rect x="64" y="64" width="896" height="896" rx="208" fill="url(#bg)"/>
  <rect x="64" y="64" width="896" height="896" rx="208" fill="url(#shine)"/>
  <rect x="64.5" y="64.5" width="895" height="895" rx="207.5" fill="none" stroke="#ffffff" stroke-opacity="0.10"/>
  <g fill="none" stroke="url(#brace)" stroke-width="64" stroke-linecap="round" stroke-linejoin="round">
    <path d="M382 268c-72 0-96 34-96 96v70c0 52-30 78-84 78 54 0 84 26 84 78v70c0 62 24 96 96 96"/>
    <path d="M642 268c72 0 96 34 96 96v70c0 52 30 78 84 78-54 0-84 26-84 78v70c0 62-24 96-96 96"/>
  </g>
  <rect x="482" y="284" width="60" height="456" rx="30" fill="url(#caret)"/>
</svg>
"""

os.makedirs(UI, exist_ok=True)
for name, body in ui.items():
    open(f"{UI}/{name}.svg", "w").write(body)
open(f"{ASSETS}/app-icon.svg", "w").write(app_icon)


# Rust source listing every bundled resource, so the binary needs no files next to it.
file_icons = sorted(f for f in os.listdir(ICONS) if f.endswith(".svg"))
entries = sorted([f"icons/{n}" for n in file_icons] + [f"ui/{n}.svg" for n in sorted(ui)])
rows = "\n".join(f'    ("{e}", include_bytes!("../assets/{e}")),' for e in entries)
font_files = sorted(f for f in os.listdir(f"{ASSETS}/fonts") if f.endswith(".ttf"))
font_rows = "\n".join(f'    include_bytes!("../assets/fonts/{f}"),' for f in font_files)
open("apps/rustcode/src/assets.rs", "w").write(f'''//! Resources compiled into the program. Generated by `scripts/generate_icons.py`.

use std::borrow::Cow;

use gpui::{{AssetSource, SharedString}};

/// File icons and interface icons, by the path the views draw them with.
const FILES: &[(&str, &[u8])] = &[
{rows}
];

/// The icon theme that ships with RustCode.
pub const ICON_THEME: &str = include_str!("../assets/icon-theme.json");

/// Fonts bundled with the program: Inter for the interface, JetBrains Mono (no ligatures, so the
/// columns of code stay exact) for code. Both are under the SIL Open Font License; see
/// `apps/rustcode/assets/fonts`.
pub const FONTS: &[&[u8]] = &[
{font_rows}
];

/// Family names of the bundled fonts.
pub const UI_FONT: &str = "Inter";
pub const CODE_FONT: &str = "JetBrains Mono NL";

/// The application icon, as a PNG, for the dock and task switcher.
pub const APP_ICON_PNG: &[u8] = include_bytes!("../assets/app-icon-512.png");

pub struct Assets;

impl AssetSource for Assets {{
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {{
        Ok(FILES.binary_search_by_key(&path, |(name, _)| *name).ok().map(|index| Cow::Borrowed(FILES[index].1)))
    }}

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {{
        Ok(FILES
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }}
}}
''')
print(len(file_icons), "file icons,", len(ui), "interface icons")
