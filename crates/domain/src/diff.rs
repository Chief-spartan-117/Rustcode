//! Differences between two texts, by line.

use similar::{ChangeTag, DiffOp, TextDiff};

/// What happened to a stretch of lines of the new text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineChangeKind {
    Added,
    Modified,
    /// Lines were removed just before `LineChange::start`; the range is empty.
    Deleted,
}

/// A run of changed lines in the new text: `start..end` (from 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineChange {
    pub kind: LineChangeKind,
    pub start: usize,
    pub end: usize,
}

/// The changed lines of `new` compared with `old`, in order.
pub fn line_changes(old: &str, new: &str) -> Vec<LineChange> {
    let diff = TextDiff::from_lines(old, new);
    let mut changes = Vec::new();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert { new_index, new_len, .. } => {
                changes.push(LineChange { kind: LineChangeKind::Added, start: new_index, end: new_index + new_len });
            }
            DiffOp::Delete { new_index, .. } => {
                changes.push(LineChange { kind: LineChangeKind::Deleted, start: new_index, end: new_index });
            }
            DiffOp::Replace { old_len, new_index, new_len, .. } => {
                // Lines that replace old ones are modified; any beyond the old count are new.
                let modified = old_len.min(new_len);
                changes.push(LineChange { kind: LineChangeKind::Modified, start: new_index, end: new_index + modified });
                if new_len > modified {
                    changes.push(LineChange {
                        kind: LineChangeKind::Added,
                        start: new_index + modified,
                        end: new_index + new_len,
                    });
                } else if old_len > new_len {
                    changes.push(LineChange {
                        kind: LineChangeKind::Deleted,
                        start: new_index + new_len,
                        end: new_index + new_len,
                    });
                }
            }
        }
    }
    changes
}

/// Kind of change at `line` of the new text, if any. `changes` is the result of `line_changes`.
pub fn change_at(changes: &[LineChange], line: usize) -> Option<LineChangeKind> {
    let at = changes.partition_point(|c| c.end.max(c.start + 1) <= line);
    let change = changes.get(at)?;
    (change.start <= line && line < change.end.max(change.start + 1)).then_some(change.kind)
}

/// A unified diff of the two texts, with `old_name` and `new_name` in its header.
pub fn unified_diff(old: &str, new: &str, old_name: &str, new_name: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    diff.unified_diff().context_radius(3).header(old_name, new_name).to_string()
}

/// For each line of `new`, the line of `old` it is unchanged from, if any.
pub fn old_line_map(old: &str, new: &str) -> Vec<Option<usize>> {
    let diff = TextDiff::from_lines(old, new);
    let mut map = vec![None; diff.new_slices().len()];
    for op in diff.ops() {
        if let DiffOp::Equal { old_index, new_index, len } = *op {
            for i in 0..len {
                map[new_index + i] = Some(old_index + i);
            }
        }
    }
    map
}

/// The hunk (header and lines) of the change from `old` to `new` that covers line `line` of
/// `new`. `None` when no hunk covers the line.
pub fn hunk_text(old: &str, new: &str, line: usize) -> Option<String> {
    let diff = TextDiff::from_lines(old, new);
    let mut unified = diff.unified_diff();
    unified.context_radius(3);
    for hunk in unified.iter_hunks() {
        let covered = hunk.ops().iter().any(|op| {
            let range = op.new_range();
            line >= range.start && line <= range.end && !matches!(op, DiffOp::Equal { .. })
        });
        if covered {
            return Some(hunk.to_string());
        }
    }
    None
}

/// `new` with the change that covers line `line` undone, taking that stretch from `old`. `None`
/// when no change covers the line.
pub fn revert_change(old: &str, new: &str, line: usize) -> Option<String> {
    let diff = TextDiff::from_lines(old, new);
    let old_lines = diff.old_slices();
    let new_lines = diff.new_slices();
    let covers = |op: &DiffOp| {
        let range = op.new_range();
        !matches!(op, DiffOp::Equal { .. }) && (range.contains(&line) || (range.is_empty() && range.start == line))
    };
    if !diff.ops().iter().any(covers) {
        return None;
    }
    let mut out = String::new();
    for op in diff.ops() {
        let lines = if covers(op) { &old_lines[op.old_range()] } else { &new_lines[op.new_range()] };
        for l in lines {
            out.push_str(l);
        }
    }
    Some(out)
}

/// One replacement that turns `old` into `new`: chars `start..end` of `old` become `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextReplace {
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// The lines of `old` the change touches, `first..last` (empty for an insertion between lines).
    pub first_line: usize,
    pub last_line: usize,
}

/// The smallest replacements that turn `old` into `new`: one per changed stretch of lines, each
/// cut down to the chars that differ, so a caret in text that did not change stays where it is.
pub fn text_edits(old: &str, new: &str) -> Vec<TextReplace> {
    let diff = TextDiff::from_lines(old, new);
    let old_lines = diff.old_slices();
    let new_lines = diff.new_slices();
    // Char offset of the start of each old line.
    let mut starts = Vec::with_capacity(old_lines.len() + 1);
    let mut at = 0;
    for line in old_lines {
        starts.push(at);
        at += line.chars().count();
    }
    starts.push(at);
    // Changed stretches as (old lines, new lines). A stretch that keeps its line count is split per
    // line, so a caller can keep only the lines it cares about.
    let mut chunks: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    for op in diff.ops() {
        if matches!(op, DiffOp::Equal { .. }) {
            continue;
        }
        let (old_range, new_range) = (op.old_range(), op.new_range());
        if old_range.len() == new_range.len() && old_range.len() > 1 {
            for i in 0..old_range.len() {
                let (o, n) = (old_range.start + i, new_range.start + i);
                if old_lines[o] != new_lines[n] {
                    chunks.push((o..o + 1, n..n + 1));
                }
            }
        } else {
            chunks.push((old_range, new_range));
        }
    }
    let mut edits = Vec::new();
    for (old_range, new_range) in chunks {
        let removed: String = old_lines[old_range.clone()].concat();
        let added: String = new_lines[new_range].concat();
        // Cut the chars the stretch keeps at both ends.
        let prefix = removed.chars().zip(added.chars()).take_while(|(a, b)| a == b).count();
        let (removed_rest, added_rest): (Vec<char>, Vec<char>) = (removed.chars().skip(prefix).collect(), added.chars().skip(prefix).collect());
        let suffix = removed_rest.iter().rev().zip(added_rest.iter().rev()).take_while(|(a, b)| a == b).count();
        let start = starts[old_range.start] + prefix;
        let end = starts[old_range.end] - suffix;
        edits.push(TextReplace {
            start,
            end,
            text: added_rest[..added_rest.len() - suffix].iter().collect(),
            first_line: old_range.start,
            last_line: old_range.end,
        });
    }
    edits
}

/// Whether the texts differ at all.
pub fn differs(old: &str, new: &str) -> bool {
    TextDiff::from_lines(old, new).iter_all_changes().any(|c| c.tag() != ChangeTag::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_modified_and_deleted_lines() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nB\nc\nx\ny\n";
        let changes = line_changes(old, new);
        assert_eq!(
            changes,
            vec![
                LineChange { kind: LineChangeKind::Modified, start: 1, end: 2 },
                LineChange { kind: LineChangeKind::Modified, start: 3, end: 4 },
                LineChange { kind: LineChangeKind::Added, start: 4, end: 5 },
            ]
        );
        assert_eq!(change_at(&changes, 1), Some(LineChangeKind::Modified));
        assert_eq!(change_at(&changes, 2), None);
        assert_eq!(change_at(&changes, 4), Some(LineChangeKind::Added));
        let removed = line_changes("a\nb\nc\n", "a\nc\n");
        assert_eq!(removed, vec![LineChange { kind: LineChangeKind::Deleted, start: 1, end: 1 }]);
        assert_eq!(change_at(&removed, 1), Some(LineChangeKind::Deleted));
    }

    #[test]
    fn old_lines_and_hunks() {
        let old = "a\nb\nc\n";
        let new = "a\nX\nc\n";
        assert_eq!(old_line_map(old, new), vec![Some(0), None, Some(2)]);
        let patch = hunk_text(old, new, 1).unwrap();
        assert!(patch.starts_with("@@"));
        assert!(patch.contains("-b\n+X\n"));
        assert_eq!(hunk_text("same\n", "same\n", 0), None);
    }

    #[test]
    fn a_change_can_be_reverted_alone() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nB\nc\nD\n";
        assert_eq!(revert_change(old, new, 1).as_deref(), Some("a\nb\nc\nD\n"));
        assert_eq!(revert_change(old, new, 3).as_deref(), Some("a\nB\nc\nd\n"));
        assert_eq!(revert_change(old, new, 2), None);
        // A deletion is put back, and an addition taken out.
        assert_eq!(revert_change("a\nb\n", "a\n", 1).as_deref(), Some("a\nb\n"));
        assert_eq!(revert_change("a\n", "a\nx\n", 1).as_deref(), Some("a\n"));
    }

    fn apply(old: &str, edits: &[TextReplace]) -> String {
        let chars: Vec<char> = old.chars().collect();
        let mut out = String::new();
        let mut at = 0;
        for e in edits {
            out.extend(&chars[at..e.start]);
            out.push_str(&e.text);
            at = e.end;
        }
        out.extend(&chars[at..]);
        out
    }

    #[test]
    fn text_edits_rebuild_the_new_text() {
        for (old, new) in [
            ("a\nb\nc\n", "a\nB\nc\n"),
            ("fn  main( ){\n}\n", "fn main() {\n}\n"),
            ("a\nb\n", "a\nb\nc\n"),
            ("a\nb\nc\n", "a\nc\n"),
            ("x", "x\n"),
            ("", "hello\n"),
            ("same\n", "same\n"),
            ("é\n  é(1)\n", "é\n    é(1)\n"),
        ] {
            assert_eq!(apply(old, &text_edits(old, new)), new, "{old:?} -> {new:?}");
        }
        assert!(text_edits("same\n", "same\n").is_empty());
    }

    #[test]
    fn text_edits_keep_what_did_not_change() {
        // Only the two added spaces change, so a caret at the end of the line is not in the edit.
        let edits = text_edits("  foo(x)\n", "    foo(x)\n");
        assert_eq!(edits.len(), 1);
        assert_eq!((edits[0].start, edits[0].end, edits[0].text.as_str()), (2, 2, "  "));
        assert_eq!((edits[0].first_line, edits[0].last_line), (0, 1));
        // Separate stretches stay separate.
        assert_eq!(text_edits("a\nb\nc\nd\n", "A\nb\nc\nD\n").len(), 2);
        // Neighbouring changed lines are told apart when the line count stays, so a caller can pick.
        let split = text_edits("a  =1;\nb  =2;\nc  =3;\n", "a = 1;\nb = 2;\nc = 3;\n");
        assert_eq!(split.iter().map(|e| (e.first_line, e.last_line)).collect::<Vec<_>>(), vec![(0, 1), (1, 2), (2, 3)]);
    }

    #[test]
    fn unified_diff_marks_lines() {
        let text = unified_diff("a\nb\n", "a\nc\n", "a/f", "b/f");
        assert!(text.contains("-b") && text.contains("+c"));
        assert!(!differs("same\n", "same\n"));
    }
}
