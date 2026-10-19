use crate::Selection;

#[derive(Debug, Clone)]
pub(crate) struct Edit {
    pub offset: usize,
    pub removed: String,
    pub removed_chars: usize,
    pub inserted: String,
    pub inserted_chars: usize,
}

/// One undo step. `edits` are in the order they were applied to the buffer.
#[derive(Debug, Clone)]
pub(crate) struct Transaction {
    pub edits: Vec<Edit>,
    pub before: Vec<Selection>,
    pub after: Vec<Selection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditKind {
    /// Single printable char; consecutive ones merge into one undo step.
    Typing,
    Other,
}

#[derive(Debug, Default)]
pub(crate) struct History {
    undo_stack: Vec<Transaction>,
    redo_stack: Vec<Transaction>,
    last_kind: Option<EditKind>,
}

fn same_positions(a: &[Selection], b: &[Selection]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.anchor == y.anchor && x.head == y.head)
}

impl History {
    pub fn record(&mut self, tx: Transaction, kind: EditKind) {
        self.redo_stack.clear();
        let can_merge = kind == EditKind::Typing
            && self.last_kind == Some(EditKind::Typing)
            && self
                .undo_stack
                .last()
                .is_some_and(|last| same_positions(&last.after, &tx.before));
        if can_merge {
            let last = self.undo_stack.last_mut().expect("checked above");
            last.edits.extend(tx.edits);
            last.after = tx.after;
        } else {
            self.undo_stack.push(tx);
        }
        self.last_kind = Some(kind);
    }

    /// Add edits to the last undo step, so one undo takes them back with it.
    pub fn amend(&mut self, edits: Vec<Edit>, after: Vec<Selection>) {
        if let Some(last) = self.undo_stack.last_mut() {
            last.edits.extend(edits);
            last.after = after;
        }
    }

    pub fn pop_undo(&mut self) -> Option<Transaction> {
        self.last_kind = None;
        self.undo_stack.pop()
    }

    pub fn push_redo(&mut self, tx: Transaction) {
        self.redo_stack.push(tx);
    }

    pub fn pop_redo(&mut self) -> Option<Transaction> {
        self.last_kind = None;
        self.redo_stack.pop()
    }

    pub fn push_undo(&mut self, tx: Transaction) {
        self.undo_stack.push(tx);
    }
}
