use std::ops::Range;

/// A caret (empty) or a range, in char offsets. `head` is the moving end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// Column kept across vertical moves through short lines.
    pub goal_column: Option<usize>,
}

impl Selection {
    pub fn caret(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
            goal_column: None,
        }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal_column: None,
        }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Sort by start and merge overlapping selections and duplicate carets.
pub(crate) fn normalize(mut selections: Vec<Selection>) -> Vec<Selection> {
    selections.sort_by_key(|s| (s.start(), s.end()));
    let mut merged: Vec<Selection> = Vec::with_capacity(selections.len());
    for next in selections {
        match merged.last_mut() {
            Some(last)
                if next.start() < last.end()
                    || (next.start() == last.end() && last.is_empty() && next.is_empty()) =>
            {
                let start = last.start();
                let end = last.end().max(next.end());
                *last = if last.head >= last.anchor {
                    Selection::new(start, end)
                } else {
                    Selection::new(end, start)
                };
            }
            _ => merged.push(next),
        }
    }
    merged
}

/// A buffer edit in char offsets: `removed` chars at `offset` were replaced by `inserted` chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharEdit {
    pub offset: usize,
    pub removed: usize,
    pub inserted: usize,
}

impl CharEdit {
    /// Where char offset `position` ends up after this edit. A position at the edit start
    /// stays before inserted text; one inside removed text moves to the edit start.
    pub fn map_offset(&self, position: usize) -> usize {
        if position <= self.offset {
            position
        } else if position >= self.offset + self.removed {
            position - self.removed + self.inserted
        } else {
            self.offset
        }
    }
}

/// Move selections along with buffer edits, for views that did not make them.
/// `edits` must be in the order they were applied to the buffer.
pub fn map_selections(selections: &[Selection], edits: &[CharEdit]) -> Vec<Selection> {
    let mapped = selections
        .iter()
        .map(|s| {
            let (mut anchor, mut head) = (s.anchor, s.head);
            for edit in edits {
                anchor = edit.map_offset(anchor);
                head = edit.map_offset(head);
            }
            Selection { anchor, head, goal_column: s.goal_column }
        })
        .collect();
    normalize(mapped)
}

#[cfg(test)]
mod map_tests {
    use super::*;

    fn edit(offset: usize, removed: usize, inserted: usize) -> CharEdit {
        CharEdit { offset, removed, inserted }
    }

    #[test]
    fn positions_after_an_edit_shift_by_its_net_length() {
        assert_eq!(edit(2, 0, 3).map_offset(5), 8);
        assert_eq!(edit(2, 4, 1).map_offset(9), 6);
    }

    #[test]
    fn positions_before_or_at_the_start_stay() {
        assert_eq!(edit(5, 0, 2).map_offset(5), 5);
        assert_eq!(edit(5, 0, 2).map_offset(1), 1);
    }

    #[test]
    fn positions_inside_removed_text_collapse_to_the_start() {
        assert_eq!(edit(5, 4, 0).map_offset(7), 5);
        assert_eq!(edit(5, 4, 2).map_offset(7), 5);
        // The end of the removed range lands after the replacement.
        assert_eq!(edit(5, 4, 2).map_offset(9), 7);
    }

    #[test]
    fn selections_follow_a_sequence_of_edits_and_merge_when_they_collide() {
        let selections = [Selection::new(10, 14), Selection::caret(20)];
        // Insert at 0, then delete 8..12 (in coordinates after the first edit).
        let mapped = map_selections(&selections, &[edit(0, 0, 2), edit(8, 4, 0)]);
        assert_eq!(mapped, vec![Selection::new(8, 12), Selection::caret(18)]);
        let collided = map_selections(&[Selection::caret(3), Selection::caret(4)], &[edit(2, 5, 0)]);
        assert_eq!(collided, vec![Selection::caret(2)]);
    }
}
