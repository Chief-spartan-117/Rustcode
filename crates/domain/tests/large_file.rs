//! A large document: how long the operations the editor does all the time take. The bounds are
//! far above what a machine needs, so a slow one does not fail them; what they catch is an
//! operation that went from a moment to seconds. Run with `--nocapture` to see the times.

use std::time::{Duration, Instant};

use rustcode_domain::{Editor, SearchOptions, Search, TextBuffer};

const LINES: usize = 1_000_000;

fn source() -> String {
    (0..LINES).map(|n| format!("let value_{n} = compute({n}, \"text {n}\");  // line {n} of a long file\n")).collect()
}

fn timed<T>(what: &str, limit: Duration, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let out = work();
    let took = started.elapsed();
    eprintln!("{what}: {took:?}");
    assert!(took < limit, "{what} took {took:?}, over {limit:?}");
    out
}

#[test]
fn a_million_lines_open_edit_undo_and_search_in_bounded_time() {
    let text = source();
    eprintln!("{} MB, {} lines", text.len() / 1_000_000, LINES);
    let buffer = timed("open (build the buffer)", Duration::from_secs(10), || TextBuffer::from_text(&text));
    assert_eq!(buffer.line_count(), LINES + 1);
    let mut editor = Editor::new(buffer);

    // Lines anywhere come at once.
    timed("read 10000 lines spread over the file", Duration::from_secs(1), || {
        for i in 0..10_000 {
            let line = (i * 97) % LINES;
            assert!(editor.buffer().line(line).is_some());
        }
    });
    timed("1000 offset-to-line lookups", Duration::from_millis(500), || {
        let len = editor.buffer().len_chars();
        for i in 0..1000 {
            let _ = editor.buffer().char_to_line((i * 7919) % len);
        }
    });

    // Typing at the start, the middle and the end.
    for (place, at) in [("start", 0usize), ("middle", editor.buffer().len_chars() / 2), ("end", editor.buffer().len_chars())] {
        editor.set_caret(at);
        timed(&format!("type 200 characters at the {place}"), Duration::from_secs(3), || {
            for _ in 0..200 {
                editor.insert_text("x");
            }
        });
    }
    timed("undo them all", Duration::from_secs(5), || {
        while editor.undo() {}
    });
    assert_eq!(editor.buffer().line_count(), LINES + 1);

    // Select all and replace is the heaviest thing a paste does.
    timed("select all and delete", Duration::from_secs(5), || {
        editor.select_all();
        editor.insert_text("");
    });
    timed("undo that", Duration::from_secs(5), || {
        editor.undo();
    });
    assert_eq!(editor.buffer().line_count(), LINES + 1);

    // Search in the whole text, as the find box does while typing.
    let searcher = Search::new("value_99999\\b", SearchOptions { regex: true, ..Default::default() }).unwrap();
    let found = timed("regex search over the file", Duration::from_secs(10), || searcher.find_all(editor.buffer()));
    assert!(!found.is_empty());
    let plain = Search::new("compute(", SearchOptions::default()).unwrap();
    let all = timed("find every match of a common word", Duration::from_secs(10), || plain.find_all(editor.buffer()));
    assert_eq!(all.len(), LINES);
}
