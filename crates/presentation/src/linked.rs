//! Linked editing: ranges a language server says change together, such as the two names of an
//! HTML tag. Typing in one of them types in the others.

use std::ops::Range;

use gpui::Context;

use crate::editor_view::EditorView;

/// The twin ranges of the place the caret is at, in char offsets of the text at `revision`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkedRanges {
    pub doc: rustcode_domain::DocumentId,
    pub revision: u64,
    /// Sorted and not overlapping.
    pub ranges: Vec<Range<usize>>,
}

/// What mirroring one edit comes to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Mirrored {
    /// Changes for the other ranges, in the coordinates of the text after the edit.
    pub replacements: Vec<(Range<usize>, String)>,
    /// The ranges after those changes.
    pub ranges: Vec<Range<usize>>,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | ':' | '.' | '-')
}

/// Carry an edit of `removed` chars at `offset` replaced by `inserted` chars over to the ranges
/// that are linked with the one it happened in. `text_of` reads the text after the edit. Returns
/// `None` when the edit is not inside one of the ranges, or leaves something in it that is not
/// part of a name (typing a space after a tag name starts the attributes, which have no twin).
pub(crate) fn mirror(
    ranges: &[Range<usize>],
    offset: usize,
    removed: usize,
    inserted: usize,
    text_of: impl Fn(Range<usize>) -> String,
) -> Option<Mirrored> {
    let edited = ranges.iter().position(|r| r.start <= offset && offset + removed <= r.end)?;
    let delta = inserted as isize - removed as isize;
    let after = |p: usize| (p as isize + delta) as usize;
    let new_edited = ranges[edited].start..after(ranges[edited].end);
    let text = text_of(new_edited);
    if !text.chars().all(is_name_char) {
        return None;
    }
    let length = text.chars().count();
    let mut replacements = Vec::new();
    let mut moved = Vec::with_capacity(ranges.len());
    let mut total: isize = 0;
    for (index, range) in ranges.iter().enumerate() {
        let now = if index > edited { after(range.start)..after(range.end) } else { range.clone() };
        if index != edited && text_of(now.clone()) != text {
            replacements.push((now.clone(), text.clone()));
        }
        let start = (range.start as isize + total) as usize;
        moved.push(start..start + length);
        total += length as isize - range.len() as isize;
    }
    Some(Mirrored { replacements, ranges: moved })
}

impl EditorView {
    /// A server's answer about the ranges linked with the caret.
    pub(crate) fn linked_ready(
        &mut self,
        doc: rustcode_domain::DocumentId,
        revision: u64,
        line: usize,
        column: usize,
        ranges: Vec<(usize, usize, usize, usize)>,
        cx: &mut Context<Self>,
    ) {
        if doc != self.document.id || revision != self.document.editor.revision() {
            return;
        }
        let buffer = self.document.editor.buffer();
        let last = buffer.line_count().saturating_sub(1);
        let offset = |line: usize, column: usize| {
            let line = line.min(last);
            buffer.line_to_char(line) + column.min(buffer.line_len_chars(line))
        };
        let mut found: Vec<Range<usize>> = ranges.iter().map(|&(l, c, el, ec)| offset(l, c)..offset(el, ec)).collect();
        found.sort_by_key(|r| r.start);
        let at = offset(line, column);
        let sound = found.len() > 1
            && found.windows(2).all(|w| w[0].end <= w[1].start)
            && found.iter().any(|r| r.start <= at && at <= r.end);
        self.features.linked = sound.then_some(LinkedRanges { doc, revision, ranges: found });
        cx.notify();
    }

    /// Whether the linked ranges held are for the text and caret as they are now.
    pub(crate) fn linked_covers_caret(&self) -> bool {
        let Some(linked) = self.features.linked.as_ref() else { return false };
        let editor = &self.document.editor;
        let caret = editor.selections().last().map_or(0, |s| s.head);
        linked.doc == self.document.id && linked.revision == editor.revision() && linked.ranges.iter().any(|r| r.start <= caret && caret <= r.end)
    }

    /// After an edit made at `revision`: type the same into the linked ranges, as part of the
    /// same undo step.
    pub(crate) fn mirror_linked(&mut self, revision: u64) {
        let Some(linked) = self.features.linked.take() else { return };
        let editor = &mut self.document.editor;
        if linked.doc != self.document.id || linked.revision != revision || editor.revision() == revision {
            // Not an edit of the text the ranges are for: they are only kept when nothing changed.
            if editor.revision() == revision {
                self.features.linked = Some(linked);
            }
            return;
        }
        let edits = editor.pending_char_edits();
        let [edit] = edits else { return };
        let (offset, removed, inserted) = (edit.offset, edit.removed, edit.inserted);
        if editor.selections().len() != 1 {
            return;
        }
        let mirrored = mirror(&linked.ranges, offset, removed, inserted, |r| editor.buffer().slice(r));
        let Some(mirrored) = mirrored else { return };
        editor.extend_last_edit(mirrored.replacements);
        self.features.linked = Some(LinkedRanges { doc: linked.doc, revision: editor.revision(), ranges: mirrored.ranges });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_in_a_name_types_in_its_twin() {
        // "<a></a>" after the user typed "b" at 2: "<ab></a>".
        let text = "<ab></a>";
        let of = |r: Range<usize>| text.chars().skip(r.start).take(r.len()).collect::<String>();
        let result = mirror(&[1..2, 6..7], 2, 0, 1, of).unwrap();
        assert_eq!(result.replacements, vec![(7..8, "ab".to_owned())]);
        assert_eq!(result.ranges, vec![1..3, 7..9]);
    }

    #[test]
    fn an_edit_in_the_second_name_changes_the_first() {
        // "<ab></ab>" after Backspace removed the "b" at 8: "<ab></a>".
        let text = "<ab></a>";
        let of = |r: Range<usize>| text.chars().skip(r.start).take(r.len()).collect::<String>();
        let result = mirror(&[1..3, 6..8], 7, 1, 0, of).unwrap();
        assert_eq!(result.replacements, vec![(1..3, "a".to_owned())]);
        assert_eq!(result.ranges, vec![1..2, 5..6]);
    }

    #[test]
    fn edits_outside_the_ranges_or_not_names_are_left_alone() {
        let text = "<a b></a>";
        let of = |r: Range<usize>| text.chars().skip(r.start).take(r.len()).collect::<String>();
        // A space typed after the name.
        assert_eq!(mirror(&[1..4, 7..8], 2, 0, 1, of), None);
        let same = |r: Range<usize>| "xxxxxxxxx".chars().skip(r.start).take(r.len()).collect::<String>();
        assert_eq!(mirror(&[1..2, 7..8], 5, 0, 1, same), None);
    }
}
