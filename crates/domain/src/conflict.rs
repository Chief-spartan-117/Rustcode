//! Merge conflict markers in a file.

/// Which side of a conflict to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The side from the current branch (above `=======`).
    Current,
    /// The side being merged in (below `=======`).
    Incoming,
    Both,
}

/// A conflict: the lines (from 0) of its `<<<<<<<`, `=======` and `>>>>>>>` markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict {
    pub start: usize,
    pub middle: usize,
    pub end: usize,
}

impl Conflict {
    pub fn contains(&self, line: usize) -> bool {
        (self.start..=self.end).contains(&line)
    }

    /// The text that replaces the whole conflict, markers included, for `choice`.
    pub fn resolved(&self, lines: &[&str], choice: Resolution) -> String {
        let current = &lines[self.start + 1..self.middle];
        let incoming = &lines[self.middle + 1..self.end];
        let kept: Vec<&str> = match choice {
            Resolution::Current => current.to_vec(),
            Resolution::Incoming => incoming.to_vec(),
            Resolution::Both => current.iter().chain(incoming).copied().collect(),
        };
        kept.iter().map(|l| format!("{l}\n")).collect()
    }
}

/// Every complete conflict in `lines`, in order.
pub fn find_conflicts(lines: &[&str]) -> Vec<Conflict> {
    let mut found = Vec::new();
    let mut open: Option<(usize, Option<usize>)> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("<<<<<<<") {
            open = Some((i, None));
        } else if line.starts_with("=======") && line.trim_end() == "=======" {
            if let Some((start, None)) = open {
                open = Some((start, Some(i)));
            }
        } else if line.starts_with(">>>>>>>") {
            if let Some((start, Some(middle))) = open {
                found.push(Conflict { start, middle, end: i });
            }
            open = None;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicts_are_found_and_resolved() {
        let text = "a\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> topic\nz";
        let lines: Vec<&str> = text.split('\n').collect();
        let conflicts = find_conflicts(&lines);
        assert_eq!(conflicts, vec![Conflict { start: 1, middle: 3, end: 5 }]);
        assert!(conflicts[0].contains(4) && !conflicts[0].contains(0));
        assert_eq!(conflicts[0].resolved(&lines, Resolution::Current), "mine\n");
        assert_eq!(conflicts[0].resolved(&lines, Resolution::Incoming), "theirs\n");
        assert_eq!(conflicts[0].resolved(&lines, Resolution::Both), "mine\ntheirs\n");
    }
}
