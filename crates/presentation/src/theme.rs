use gpui::{FontStyle, FontWeight, HighlightStyle, Hsla, Rgba};
use rustcode_domain::{Color, ColorTheme, TokenStyle};

/// A theme color as the window draws it.
pub(crate) fn rgba_of(color: Color) -> Rgba {
    to_rgba(color)
}

fn to_rgba(color: Color) -> Rgba {
    Rgba {
        r: color.r as f32 / 255.0,
        g: color.g as f32 / 255.0,
        b: color.b as f32 / 255.0,
        a: color.a as f32 / 255.0,
    }
}

/// Fully transparent: "no color of its own", so what is behind shows.
const CLEAR: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

fn hex(value: u32) -> Rgba {
    gpui::rgb(value)
}

/// The usual 16 ANSI colors, as in VS Code's dark theme.
pub(crate) const ANSI: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666, 0xf14c4c, 0x23d18b,
    0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
];

/// `terminal.ansi...` keys, in the order of [`ANSI`].
const ANSI_KEYS: [&str; 16] = [
    "terminal.ansiBlack",
    "terminal.ansiRed",
    "terminal.ansiGreen",
    "terminal.ansiYellow",
    "terminal.ansiBlue",
    "terminal.ansiMagenta",
    "terminal.ansiCyan",
    "terminal.ansiWhite",
    "terminal.ansiBrightBlack",
    "terminal.ansiBrightRed",
    "terminal.ansiBrightGreen",
    "terminal.ansiBrightYellow",
    "terminal.ansiBrightBlue",
    "terminal.ansiBrightMagenta",
    "terminal.ansiBrightCyan",
    "terminal.ansiBrightWhite",
];

pub struct ActivityBar {
    pub background: Rgba,
    pub foreground: Rgba,
    pub inactive_foreground: Rgba,
    pub active_border: Rgba,
    pub active_background: Rgba,
    pub border: Rgba,
}

pub struct SideBar {
    pub background: Rgba,
    pub foreground: Rgba,
    pub border: Rgba,
    pub title_foreground: Rgba,
    pub section_background: Rgba,
    pub section_foreground: Rgba,
}

/// Lists and trees: the explorer, search results and the like.
pub struct Lists {
    pub active_selection: Rgba,
    pub active_selection_foreground: Option<Rgba>,
    pub inactive_selection: Rgba,
    pub hover: Rgba,
}

pub struct Tabs {
    /// The strip behind the tabs.
    pub bar_background: Rgba,
    pub active_background: Rgba,
    pub inactive_background: Rgba,
    pub active_foreground: Rgba,
    pub inactive_foreground: Rgba,
    /// Line between tabs, when the theme draws one.
    pub border: Option<Rgba>,
    /// Line above the active tab of the active group.
    pub active_border_top: Rgba,
    /// Line above the active tab of a group that does not have the keyboard.
    pub unfocused_active_border_top: Rgba,
}

pub struct StatusBar {
    pub background: Rgba,
    pub foreground: Rgba,
    pub border: Rgba,
    pub hover: Rgba,
}

pub struct PanelColors {
    pub background: Rgba,
    pub border: Rgba,
    pub title_active: Rgba,
    pub title_inactive: Rgba,
    pub title_active_border: Rgba,
}

pub struct MenuColors {
    pub background: Rgba,
    pub foreground: Rgba,
    pub selection: Rgba,
    pub selection_foreground: Option<Rgba>,
    pub separator: Rgba,
    pub border: Rgba,
    /// Title of the menu bar that is open or has the keyboard.
    pub bar_selection: Rgba,
}

/// The command palette and other pickers.
pub struct QuickInput {
    pub background: Rgba,
    pub foreground: Rgba,
    pub focus_background: Rgba,
}

pub struct TerminalColors {
    pub background: Rgba,
    pub foreground: Rgba,
    pub selection: Rgba,
    pub cursor: Rgba,
    pub ansi: [Rgba; 16],
}

/// Colors of problems, in the editor and in lists.
pub struct Markers {
    pub error: Rgba,
    pub warning: Rgba,
    pub info: Rgba,
}

/// Colors of files that git reports on.
pub struct GitColors {
    pub modified: Rgba,
    pub added: Rgba,
    pub untracked: Rgba,
    pub deleted: Rgba,
    pub conflicting: Rgba,
    pub ignored: Rgba,
}

pub struct GutterColors {
    pub background: Rgba,
    /// Marks of lines changed since the last commit.
    pub added: Rgba,
    pub modified: Rgba,
    pub deleted: Rgba,
    /// Line number of the line with the caret.
    pub active_line_number: Rgba,
}

/// Tints of diffs and merge conflicts.
pub struct DiffColors {
    pub inserted_line: Rgba,
    pub removed_line: Rgba,
    pub header_line: Rgba,
    pub current_header: Rgba,
    pub current_content: Rgba,
    pub incoming_header: Rgba,
    pub incoming_content: Rgba,
}

/// Breakpoints, and the line a stopped program is on.
pub struct DebugColors {
    pub breakpoint: Rgba,
    /// Dimmed breakpoint: the debugger could not place it.
    pub breakpoint_unverified: Rgba,
    pub stack_line: Rgba,
    pub stack_arrow: Rgba,
}

pub struct InlayHintColors {
    pub foreground: Rgba,
    pub background: Rgba,
}

pub struct ScrollbarColors {
    pub slider: Rgba,
    pub slider_hover: Rgba,
    pub slider_active: Rgba,
}

pub struct BadgeColors {
    /// Count on an activity bar icon.
    pub activity_background: Rgba,
    pub activity_foreground: Rgba,
    /// Count beside a label, such as the number of problems.
    pub background: Rgba,
    pub foreground: Rgba,
}

/// Messages that pop up from extensions and tools.
pub struct NotificationColors {
    pub background: Rgba,
    pub foreground: Rgba,
    pub border: Rgba,
    pub link: Rgba,
    pub info_icon: Rgba,
    pub warning_icon: Rgba,
    pub error_icon: Rgba,
}

pub struct BreadcrumbColors {
    pub background: Rgba,
    pub foreground: Rgba,
    /// The last part, where the caret is.
    pub focus_foreground: Rgba,
}

/// Text inputs and the option buttons inside them.
pub struct InputColors {
    pub foreground: Rgba,
    pub border: Option<Rgba>,
    pub placeholder: Rgba,
    pub option_active_border: Option<Rgba>,
    pub option_active_background: Rgba,
    pub error_background: Rgba,
    pub error_border: Rgba,
}

/// The window frame and the title bar.
pub struct WindowColors {
    pub title_inactive_background: Rgba,
    pub title_inactive_foreground: Rgba,
    pub title_border: Option<Rgba>,
    pub active_border: Option<Rgba>,
    pub inactive_border: Option<Rgba>,
}

/// The hover box and the completion list.
pub struct WidgetColors {
    pub hover_background: Rgba,
    pub hover_foreground: Rgba,
    pub hover_border: Rgba,
    pub suggest_background: Rgba,
    pub suggest_foreground: Rgba,
    pub suggest_border: Rgba,
    pub suggest_selected: Rgba,
    /// The part of a suggestion that matches what was typed.
    pub suggest_highlight: Rgba,
}

/// Brackets, the pair next to the caret, and the vertical guides.
pub struct BracketColors {
    /// Colors by nesting depth, cycling. Never empty.
    pub levels: Vec<Rgba>,
    /// A bracket with no partner.
    pub unexpected: Rgba,
    /// Behind the two brackets of the pair next to the caret.
    pub match_background: Rgba,
    pub match_border: Rgba,
    /// A bracket pair's guide where the theme names one, by level; otherwise the level's color,
    /// faint (and full for the active pair).
    pub pair_guide: [Option<Rgba>; 6],
    pub pair_guide_active: [Option<Rgba>; 6],
    pub indent_guide: Rgba,
    pub indent_guide_active: Rgba,
}

impl BracketColors {
    /// Color of the brackets at nesting `depth`.
    pub fn level(&self, depth: usize) -> Rgba {
        self.levels[depth % self.levels.len()]
    }

    /// Color of the guide of a pair at nesting `depth`.
    pub fn guide(&self, depth: usize, active: bool) -> Rgba {
        let slot = depth % self.levels.len();
        let named = if active { self.pair_guide_active.get(slot) } else { self.pair_guide.get(slot) };
        match named.copied().flatten() {
            Some(color) => color,
            None => {
                let base = self.levels[slot];
                Rgba { a: if active { 1.0 } else { 0.3 }, ..base }
            }
        }
    }
}

pub struct ButtonColors {
    pub background: Rgba,
    pub foreground: Rgba,
    pub hover: Rgba,
}

/// GPUI-ready colors and text styles derived from a domain [`ColorTheme`].
///
/// The first group is what the editor always had. The rest are VS Code workbench colors, each
/// read from the theme file by its key (`sideBar.background`, ...) and falling back to the look
/// the program had before it read them, so a theme that does not set one changes nothing.
pub struct Theme {
    pub background: Rgba,
    pub gutter_text: Rgba,
    pub text: Rgba,
    pub title_bar: Rgba,
    pub title_bar_text: Rgba,
    pub selection: Rgba,
    pub caret: Rgba,
    pub find_match: Rgba,
    pub find_match_highlight: Rgba,
    pub line_highlight: Rgba,
    pub border: Rgba,
    pub widget_background: Rgba,
    pub input_background: Rgba,
    pub activity_bar: ActivityBar,
    pub side_bar: SideBar,
    pub lists: Lists,
    pub tabs: Tabs,
    pub status_bar: StatusBar,
    pub panel: PanelColors,
    pub menu: MenuColors,
    pub quick_input: QuickInput,
    pub terminal: TerminalColors,
    pub markers: Markers,
    pub git: GitColors,
    pub gutter: GutterColors,
    pub diff: DiffColors,
    pub button: ButtonColors,
    pub widgets: WidgetColors,
    pub brackets: BracketColors,
    pub scrollbar: ScrollbarColors,
    /// Underlined links in the text.
    pub text_link: Rgba,
    /// Notes a language server has the editor show in the text.
    pub inlay_hint: InlayHintColors,
    pub debug: DebugColors,
    pub badge: BadgeColors,
    pub notifications: NotificationColors,
    pub breadcrumbs: BreadcrumbColors,
    pub input: InputColors,
    pub window: WindowColors,
    /// Lines between editor groups.
    pub group_border: Rgba,
    /// Outline of the focused widget.
    pub focus_border: Rgba,
}

impl Theme {
    pub fn from_domain(theme: &ColorTheme) -> Self {
        let colors = &theme.colors;
        let named = |key: &str| theme.color(key).map(to_rgba);
        // The first key that the theme defines, else `fallback`.
        let pick = |keys: &[&str], fallback: Rgba| keys.iter().find_map(|k| named(k)).unwrap_or(fallback);

        let background = to_rgba(colors.background);
        let text = to_rgba(colors.foreground);
        let gutter_text = to_rgba(colors.gutter_foreground);
        let title_bar = to_rgba(colors.title_bar_background);
        let title_bar_text = to_rgba(colors.title_bar_foreground);
        let selection = to_rgba(colors.selection);
        let caret = to_rgba(colors.caret);
        let border = to_rgba(colors.border);
        let widget_background = to_rgba(colors.widget_background);

        let tab_bar = pick(&["editorGroupHeader.tabsBackground"], title_bar);
        let hover = pick(&["list.hoverBackground"], selection);
        let mut ansi = [CLEAR; 16];
        for (index, slot) in ansi.iter_mut().enumerate() {
            *slot = pick(&[ANSI_KEYS[index]], hex(ANSI[index]));
        }
        let button_background = pick(&["button.background"], selection);
        Self {
            background,
            gutter_text,
            text,
            title_bar,
            title_bar_text,
            selection,
            caret,
            find_match: to_rgba(colors.find_match),
            find_match_highlight: to_rgba(colors.find_match_highlight),
            line_highlight: to_rgba(colors.line_highlight),
            border,
            widget_background,
            input_background: to_rgba(colors.input_background),
            activity_bar: ActivityBar {
                background: pick(&["activityBar.background"], title_bar),
                foreground: pick(&["activityBar.foreground"], title_bar_text),
                inactive_foreground: pick(&["activityBar.inactiveForeground"], gutter_text),
                active_border: pick(&["activityBar.activeBorder", "focusBorder"], caret),
                active_background: pick(&["activityBar.activeBackground"], CLEAR),
                border: pick(&["activityBar.border"], border),
            },
            side_bar: SideBar {
                background: pick(&["sideBar.background"], widget_background),
                foreground: pick(&["sideBar.foreground"], text),
                border: pick(&["sideBar.border"], title_bar),
                title_foreground: pick(&["sideBarTitle.foreground"], gutter_text),
                section_background: pick(&["sideBarSectionHeader.background"], CLEAR),
                section_foreground: pick(&["sideBarSectionHeader.foreground"], gutter_text),
            },
            lists: Lists {
                active_selection: pick(&["list.activeSelectionBackground"], selection),
                active_selection_foreground: named("list.activeSelectionForeground"),
                inactive_selection: pick(&["list.inactiveSelectionBackground"], selection),
                hover,
            },
            tabs: Tabs {
                bar_background: tab_bar,
                active_background: pick(&["tab.activeBackground"], background),
                inactive_background: pick(&["tab.inactiveBackground"], CLEAR),
                active_foreground: pick(&["tab.activeForeground"], text),
                inactive_foreground: pick(&["tab.inactiveForeground"], gutter_text),
                border: named("tab.border"),
                active_border_top: pick(&["tab.activeBorderTop", "tab.activeBorder"], caret),
                unfocused_active_border_top: pick(&["tab.unfocusedActiveBorderTop"], tab_bar),
            },
            status_bar: StatusBar {
                background: pick(&["statusBar.background"], title_bar),
                foreground: pick(&["statusBar.foreground"], title_bar_text),
                border: pick(&["statusBar.border"], border),
                hover: pick(&["statusBarItem.hoverBackground"], selection),
            },
            panel: PanelColors {
                background: pick(&["panel.background"], widget_background),
                border: pick(&["panel.border"], border),
                title_active: pick(&["panelTitle.activeForeground"], text),
                title_inactive: pick(&["panelTitle.inactiveForeground"], gutter_text),
                title_active_border: pick(&["panelTitle.activeBorder"], caret),
            },
            menu: MenuColors {
                background: pick(&["menu.background"], widget_background),
                foreground: pick(&["menu.foreground"], title_bar_text),
                selection: pick(&["menu.selectionBackground"], selection),
                selection_foreground: named("menu.selectionForeground"),
                separator: pick(&["menu.separatorBackground"], selection),
                border: pick(&["menu.border"], selection),
                bar_selection: pick(&["menubar.selectionBackground"], selection),
            },
            quick_input: QuickInput {
                background: pick(&["quickInput.background"], widget_background),
                foreground: pick(&["quickInput.foreground"], text),
                focus_background: pick(&["quickInputList.focusBackground"], selection),
            },
            terminal: TerminalColors {
                background: pick(&["terminal.background"], background),
                foreground: pick(&["terminal.foreground"], text),
                selection: pick(&["terminal.selectionBackground"], selection),
                cursor: pick(&["terminalCursor.foreground"], caret),
                ansi,
            },
            markers: Markers {
                error: pick(&["editorError.foreground"], hex(0xf14c4c)),
                warning: pick(&["editorWarning.foreground"], hex(0xcca700)),
                info: pick(&["editorInfo.foreground"], hex(0x3794ff)),
            },
            git: GitColors {
                modified: pick(&["gitDecoration.modifiedResourceForeground"], hex(0xe2c08d)),
                added: pick(&["gitDecoration.addedResourceForeground"], hex(0x73c991)),
                untracked: pick(&["gitDecoration.untrackedResourceForeground"], hex(0x73c991)),
                deleted: pick(&["gitDecoration.deletedResourceForeground"], hex(0xc74e39)),
                conflicting: pick(&["gitDecoration.conflictingResourceForeground"], hex(0xe51400)),
                ignored: pick(&["gitDecoration.ignoredResourceForeground"], gutter_text),
            },
            gutter: GutterColors {
                background: pick(&["editorGutter.background"], background),
                added: pick(&["editorGutter.addedBackground"], hex(0x2ea043)),
                modified: pick(&["editorGutter.modifiedBackground"], hex(0x1b81a8)),
                deleted: pick(&["editorGutter.deletedBackground"], hex(0xf85149)),
                active_line_number: pick(&["editorLineNumber.activeForeground"], text),
            },
            diff: DiffColors {
                inserted_line: pick(&["diffEditor.insertedLineBackground", "diffEditor.insertedTextBackground"], gpui::rgba(0x2ea04326)),
                removed_line: pick(&["diffEditor.removedLineBackground", "diffEditor.removedTextBackground"], gpui::rgba(0xf8514926)),
                header_line: gpui::rgba(0x1b81a826),
                current_header: pick(&["merge.currentHeaderBackground"], gpui::rgba(0xe5c07b44)),
                current_content: pick(&["merge.currentContentBackground"], gpui::rgba(0x2ea04330)),
                incoming_header: pick(&["merge.incomingHeaderBackground"], gpui::rgba(0xe5c07b44)),
                incoming_content: pick(&["merge.incomingContentBackground"], gpui::rgba(0x1b81a830)),
            },
            button: ButtonColors {
                background: button_background,
                foreground: pick(&["button.foreground"], text),
                hover: pick(&["button.hoverBackground"], button_background),
            },
            widgets: WidgetColors {
                hover_background: pick(&["editorHoverWidget.background", "editorWidget.background"], widget_background),
                hover_foreground: pick(&["editorHoverWidget.foreground"], text),
                hover_border: pick(&["editorHoverWidget.border", "editorWidget.border"], border),
                suggest_background: pick(&["editorSuggestWidget.background", "editorWidget.background"], widget_background),
                suggest_foreground: pick(&["editorSuggestWidget.foreground"], text),
                suggest_border: pick(&["editorSuggestWidget.border", "editorWidget.border"], border),
                suggest_selected: pick(&["editorSuggestWidget.selectedBackground", "quickInputList.focusBackground"], selection),
                suggest_highlight: pick(&["editorSuggestWidget.highlightForeground"], hex(0x18a3ff)),
            },
            brackets: {
                let dark = theme.kind == rustcode_domain::ThemeKind::Dark;
                let defaults: [Option<u32>; 6] = if dark {
                    [Some(0xffd700), Some(0xda70d6), Some(0x179fff), None, None, None]
                } else {
                    [Some(0x0431fa), Some(0x319331), Some(0x7b3814), None, None, None]
                };
                // A level the theme leaves transparent is skipped, as in VS Code.
                let mut levels: Vec<Rgba> = (0..6)
                    .filter_map(|i| {
                        named(&format!("editorBracketHighlight.foreground{}", i + 1)).or(defaults[i].map(hex))
                    })
                    .filter(|c| c.a > 0.0)
                    .collect();
                if levels.is_empty() {
                    levels.push(text);
                }
                let numbered = |prefix: &str| -> [Option<Rgba>; 6] {
                    std::array::from_fn(|i| named(&format!("{prefix}{}", i + 1)).filter(|c| c.a > 0.0))
                };
                BracketColors {
                    levels,
                    unexpected: pick(
                        &["editorBracketHighlight.unexpectedBracket.foreground"],
                        Rgba { r: 1.0, g: 18.0 / 255.0, b: 18.0 / 255.0, a: 0.8 },
                    ),
                    match_background: pick(&["editorBracketMatch.background"], Rgba { r: 0.0, g: 100.0 / 255.0, b: 0.0, a: 0.1 }),
                    match_border: pick(&["editorBracketMatch.border"], if dark { hex(0x888888) } else { hex(0xb9b9b9) }),
                    pair_guide: numbered("editorBracketPairGuide.background"),
                    pair_guide_active: numbered("editorBracketPairGuide.activeBackground"),
                    indent_guide: pick(
                        &["editorIndentGuide.background1", "editorIndentGuide.background"],
                        if dark { hex(0x404040) } else { hex(0xd3d3d3) },
                    ),
                    indent_guide_active: pick(
                        &["editorIndentGuide.activeBackground1", "editorIndentGuide.activeBackground"],
                        if dark { hex(0x707070) } else { hex(0x939393) },
                    ),
                }
            },
            text_link: pick(&["textLink.foreground"], hex(0x3794ff)),
            debug: DebugColors {
                breakpoint: pick(&["debugIcon.breakpointForeground"], hex(0xe51400)),
                breakpoint_unverified: pick(&["debugIcon.breakpointUnverifiedForeground"], hex(0x848484)),
                stack_line: pick(&["editor.stackFrameHighlightBackground"], Rgba { r: 1.0, g: 1.0, b: 0.0, a: 0.2 }),
                stack_arrow: pick(&["debugIcon.breakpointCurrentStackframeForeground"], hex(0xffcc00)),
            },
            inlay_hint: InlayHintColors {
                foreground: pick(&["editorInlayHint.foreground"], hex(0x969696)),
                background: pick(&["editorInlayHint.background"], Rgba { r: 0.5, g: 0.5, b: 0.5, a: 0.18 }),
            },
            scrollbar: ScrollbarColors {
                slider: pick(&["scrollbarSlider.background"], Rgba { r: 0.47, g: 0.47, b: 0.47, a: 0.4 }),
                slider_hover: pick(&["scrollbarSlider.hoverBackground"], Rgba { r: 0.39, g: 0.39, b: 0.39, a: 0.7 }),
                slider_active: pick(&["scrollbarSlider.activeBackground"], Rgba { r: 0.75, g: 0.75, b: 0.75, a: 0.4 }),
            },
            badge: BadgeColors {
                activity_background: pick(&["activityBarBadge.background"], hex(0x007acc)),
                activity_foreground: pick(&["activityBarBadge.foreground"], hex(0xffffff)),
                background: pick(&["badge.background"], hex(0x4d4d4d)),
                foreground: pick(&["badge.foreground"], hex(0xffffff)),
            },
            notifications: NotificationColors {
                background: pick(&["notifications.background"], widget_background),
                foreground: pick(&["notifications.foreground"], text),
                border: pick(&["notificationToast.border", "notifications.border"], border),
                link: pick(&["notificationLink.foreground"], hex(0x3794ff)),
                info_icon: pick(&["notificationsInfoIcon.foreground"], hex(0x3794ff)),
                warning_icon: pick(&["notificationsWarningIcon.foreground"], hex(0xcca700)),
                error_icon: pick(&["notificationsErrorIcon.foreground"], hex(0xf14c4c)),
            },
            breadcrumbs: BreadcrumbColors {
                background: pick(&["breadcrumb.background"], background),
                foreground: pick(&["breadcrumb.foreground"], gutter_text),
                focus_foreground: pick(&["breadcrumb.focusForeground", "breadcrumb.activeSelectionForeground"], text),
            },
            input: InputColors {
                foreground: pick(&["input.foreground"], text),
                border: named("input.border"),
                placeholder: pick(&["input.placeholderForeground"], gutter_text),
                option_active_border: named("inputOption.activeBorder"),
                option_active_background: pick(&["inputOption.activeBackground"], pick(&["button.background"], selection)),
                error_background: pick(&["inputValidation.errorBackground"], hex(0x5a1d1d)),
                error_border: pick(&["inputValidation.errorBorder"], hex(0xbe1100)),
            },
            window: WindowColors {
                title_inactive_background: pick(&["titleBar.inactiveBackground"], title_bar),
                title_inactive_foreground: pick(&["titleBar.inactiveForeground"], gutter_text),
                title_border: named("titleBar.border"),
                active_border: named("window.activeBorder"),
                inactive_border: named("window.inactiveBorder"),
            },
            group_border: pick(&["editorGroup.border"], border),
            focus_border: pick(&["focusBorder"], caret),
        }
    }

    pub fn highlight_style(&self, style: TokenStyle) -> HighlightStyle {
        HighlightStyle {
            color: style.foreground.map(|c| Hsla::from(to_rgba(c))),
            font_weight: style.bold.then_some(FontWeight::BOLD),
            font_style: style.italic.then_some(FontStyle::Italic),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustcode_domain::ColorTheme;

    fn close(a: Rgba, hex: u32) -> bool {
        let b = gpui::rgb(hex);
        (a.r - b.r).abs() < 0.01 && (a.g - b.g).abs() < 0.01 && (a.b - b.b).abs() < 0.01
    }

    #[test]
    fn a_theme_without_workbench_colors_keeps_the_old_look() {
        let domain = ColorTheme::dark_plus();
        let theme = Theme::from_domain(&domain);
        assert_eq!(theme.side_bar.background, theme.widget_background);
        assert_eq!(theme.activity_bar.background, theme.title_bar);
        assert_eq!(theme.status_bar.background, theme.title_bar);
        assert_eq!(theme.tabs.active_background, theme.background);
        assert_eq!(theme.tabs.bar_background, theme.title_bar);
        assert_eq!(theme.panel.background, theme.widget_background);
        assert_eq!(theme.lists.hover, theme.selection);
        assert_eq!(theme.terminal.background, theme.background);
        assert!(close(theme.terminal.ansi[1], 0xcd3131));
        assert!(close(theme.markers.error, 0xf14c4c));
        assert!(close(theme.git.modified, 0xe2c08d));
    }

    #[test]
    fn named_colors_replace_the_fallbacks() {
        let mut domain = ColorTheme::dark_plus();
        for (key, value) in [
            ("sideBar.background", 0x111111),
            ("activityBar.background", 0x222222),
            ("statusBar.background", 0x333333),
            ("tab.activeBackground", 0x444444),
            ("editorGroupHeader.tabsBackground", 0x555555),
            ("panel.background", 0x666666),
            ("list.hoverBackground", 0x777777),
            ("terminal.ansiRed", 0x888888),
            ("editorError.foreground", 0x999999),
            ("gitDecoration.modifiedResourceForeground", 0xaaaaaa),
            ("editorGutter.addedBackground", 0xbbbbbb),
            ("menu.selectionBackground", 0xcccccc),
        ] {
            domain.set_color(key, rustcode_domain::Color::rgb(value));
        }
        let theme = Theme::from_domain(&domain);
        assert!(close(theme.side_bar.background, 0x111111));
        assert!(close(theme.activity_bar.background, 0x222222));
        assert!(close(theme.status_bar.background, 0x333333));
        assert!(close(theme.tabs.active_background, 0x444444));
        assert!(close(theme.tabs.bar_background, 0x555555));
        assert!(close(theme.panel.background, 0x666666));
        assert!(close(theme.lists.hover, 0x777777));
        assert!(close(theme.terminal.ansi[1], 0x888888));
        assert!(close(theme.markers.error, 0x999999));
        assert!(close(theme.git.modified, 0xaaaaaa));
        assert!(close(theme.gutter.added, 0xbbbbbb));
        assert!(close(theme.menu.selection, 0xcccccc));
        // Colors not named stay as they were.
        assert!(close(theme.markers.warning, 0xcca700));
        assert_eq!(theme.status_bar.foreground, theme.title_bar_text);
    }

    #[test]
    fn the_remaining_workbench_colors_have_fallbacks_and_can_be_named() {
        let plain = Theme::from_domain(&ColorTheme::dark_plus());
        assert!(close(plain.badge.activity_background, 0x007acc));
        assert!(plain.input.border.is_none() && plain.window.active_border.is_none());
        assert_eq!(plain.breadcrumbs.focus_foreground, plain.text);
        assert_eq!(plain.window.title_inactive_background, plain.title_bar);
        assert!(plain.scrollbar.slider.a > 0.0 && plain.scrollbar.slider.a < 1.0);

        let mut domain = ColorTheme::dark_plus();
        for (key, value) in [
            ("scrollbarSlider.background", 0x010101),
            ("activityBarBadge.background", 0x020202),
            ("badge.background", 0x030303),
            ("notifications.background", 0x040404),
            ("notificationsErrorIcon.foreground", 0x050505),
            ("breadcrumb.foreground", 0x060606),
            ("input.border", 0x070707),
            ("inputValidation.errorBorder", 0x080808),
            ("titleBar.inactiveBackground", 0x090909),
            ("window.activeBorder", 0x0a0a0a),
        ] {
            domain.set_color(key, rustcode_domain::Color::rgb(value));
        }
        let theme = Theme::from_domain(&domain);
        assert!(close(theme.scrollbar.slider, 0x010101));
        assert!(close(theme.badge.activity_background, 0x020202));
        assert!(close(theme.badge.background, 0x030303));
        assert!(close(theme.notifications.background, 0x040404));
        assert!(close(theme.notifications.error_icon, 0x050505));
        assert!(close(theme.breadcrumbs.foreground, 0x060606));
        assert!(close(theme.input.border.unwrap(), 0x070707));
        assert!(close(theme.input.error_border, 0x080808));
        assert!(close(theme.window.title_inactive_background, 0x090909));
        assert!(close(theme.window.active_border.unwrap(), 0x0a0a0a));
    }

    #[test]
    fn the_first_of_several_keys_wins() {
        let mut domain = ColorTheme::dark_plus();
        domain.set_color("tab.activeBorder", rustcode_domain::Color::rgb(0x112233));
        assert!(close(Theme::from_domain(&domain).tabs.active_border_top, 0x112233));
        domain.set_color("tab.activeBorderTop", rustcode_domain::Color::rgb(0x445566));
        assert!(close(Theme::from_domain(&domain).tabs.active_border_top, 0x445566));
    }
}
