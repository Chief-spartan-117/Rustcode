//! Time-sliced highlighting on a large document with the real TextMate engine.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustcode_application::{IncrementalHighlighter, SyntaxHighlighter};
use rustcode_domain::{ColorTheme, Editor, Language, TextBuffer};
use rustcode_infrastructure::TextMateHighlighter;

const LINES: usize = 20_000;
const SLICE: Duration = Duration::from_millis(4);

fn rust_source() -> String {
    (1..=LINES)
        .map(|n| format!("fn generated_{n}(input: usize) -> usize {{ input.wrapping_mul({n}) }}\n"))
        .collect()
}

fn highlighter(theme: &Arc<ColorTheme>) -> IncrementalHighlighter {
    let engine = TextMateHighlighter::new().expect("engine");
    let session = engine.start_session(&Language::new("rust")).expect("rust session");
    IncrementalHighlighter::new(session, theme.clone())
}

/// Run slices until idle; returns (slice count, slowest slice).
fn drain(h: &mut IncrementalHighlighter, buffer: &TextBuffer) -> (usize, Duration) {
    let (mut slices, mut worst) = (0, Duration::ZERO);
    loop {
        let started = Instant::now();
        let done = h.advance(buffer, SLICE);
        worst = worst.max(started.elapsed());
        slices += 1;
        if done {
            return (slices, worst);
        }
    }
}

#[test]
fn large_document_is_tokenized_in_bounded_slices() {
    let theme = Arc::new(ColorTheme::dark_plus());
    let mut editor = Editor::new(TextBuffer::from_text(&rust_source()));
    let mut h = highlighter(&theme);

    let (slices, worst) = drain(&mut h, editor.buffer());
    assert!(h.is_idle(editor.buffer()));
    assert!(slices > 1, "whole document must not be tokenized in one slice");
    // Slices stop at the budget plus at most one line; generous bound for slow CI.
    assert!(worst < Duration::from_millis(250), "slice took {worst:?}");
    assert!(!h.line(0).is_empty());
    assert!(!h.line(LINES - 1).is_empty());
    let original = h.line(LINES - 1).to_vec();

    // Opening a block comment on line 0 restyles every later line.
    editor.move_to_document_start(false);
    editor.insert_text("/* ");
    h.apply_changes(&editor.take_changes());
    assert!(!h.is_idle(editor.buffer()));
    drain(&mut h, editor.buffer());
    assert!(h.is_idle(editor.buffer()));
    assert_ne!(h.line(LINES - 1), original.as_slice());

    // Undo restores the original styling of the last line.
    editor.undo();
    h.apply_changes(&editor.take_changes());
    drain(&mut h, editor.buffer());
    assert_eq!(h.line(LINES - 1), original.as_slice());
}
