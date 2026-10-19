//! Command registry: named, searchable actions shown in the command palette.
//!
//! The registry only describes commands. Presentation maps a [`CommandId`] to behavior.

/// Stable identifier of a command, e.g. `"file.save"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandId(pub &'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: CommandId,
    /// Shown in the palette, e.g. `"File: Save"`.
    pub title: String,
    /// Display text of the default key binding, e.g. `"cmd-s"`.
    pub keybinding: Option<String>,
}

/// Score `text` against `query` as a case-insensitive subsequence match. `None` when the
/// query is not a subsequence. Higher is better: consecutive characters and matches at word
/// starts score more, and earlier matches score more. An empty query matches everything.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let mut query_chars = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase);
    let mut wanted = query_chars.next();
    if wanted.is_none() {
        return Some(0);
    }
    let mut score = 0;
    let mut previous_matched = false;
    let mut previous: Option<char> = None;
    for (index, c) in text.chars().enumerate() {
        let lower = c.to_lowercase().next().unwrap_or(c);
        if Some(lower) == wanted {
            let word_start = previous.map_or(true, |p| !p.is_alphanumeric());
            score += 10;
            if previous_matched {
                score += 15;
            }
            if word_start {
                score += 12;
            }
            score -= (index as i32).min(20) / 4;
            previous_matched = true;
            wanted = query_chars.next();
            if wanted.is_none() {
                return Some(score);
            }
        } else {
            previous_matched = false;
        }
        previous = Some(c);
    }
    None
}

/// Indices into `items` of the entries matching `query`, best first. Ties keep input order.
pub fn rank<'a>(query: &str, titles: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = titles
        .into_iter()
        .enumerate()
        .filter_map(|(index, title)| fuzzy_score(query, title).map(|score| (score, index)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, index)| index).collect()
}

#[derive(Debug, Default)]
pub struct CommandRegistry {
    commands: Vec<Command>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a command. A command with the same id replaces the earlier one.
    pub fn register(&mut self, id: CommandId, title: &str, keybinding: Option<&str>) {
        let command = Command {
            id,
            title: title.to_owned(),
            keybinding: keybinding.map(str::to_owned),
        };
        match self.commands.iter_mut().find(|c| c.id == id) {
            Some(existing) => *existing = command,
            None => self.commands.push(command),
        }
    }

    pub fn get(&self, id: CommandId) -> Option<&Command> {
        self.commands.iter().find(|c| c.id == id)
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Commands matching `query`, best match first. An empty query lists all in registration order.
    pub fn search(&self, query: &str) -> Vec<&Command> {
        rank(query, self.commands.iter().map(|c| c.title.as_str()))
            .into_iter()
            .map(|index| &self.commands[index])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matches_case_insensitively() {
        assert!(fuzzy_score("fsv", "File: Save").is_some());
        assert!(fuzzy_score("SAVE", "File: Save").is_some());
        assert!(fuzzy_score("vsf", "File: Save").is_none());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("a b", "a  b"), fuzzy_score("ab", "a  b"));
    }

    #[test]
    fn word_starts_and_runs_rank_higher() {
        let ranked = rank("fs", ["Refresh status", "Find: Selection"]);
        assert_eq!(ranked[0], 1);
        let ranked = rank("save", ["Select All Various Edits", "File: Save"]);
        assert_eq!(ranked[0], 1);
    }

    #[test]
    fn ties_keep_registration_order() {
        assert_eq!(rank("", ["b", "a", "c"]), vec![0, 1, 2]);
    }

    #[test]
    fn registry_register_replace_search() {
        let mut registry = CommandRegistry::new();
        registry.register(CommandId("file.save"), "File: Save", Some("cmd-s"));
        registry.register(CommandId("edit.undo"), "Edit: Undo", Some("cmd-z"));
        registry.register(CommandId("file.save"), "File: Save Document", None);
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.get(CommandId("file.save")).unwrap().title, "File: Save Document");
        let found = registry.search("undo");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, CommandId("edit.undo"));
        assert_eq!(registry.search("").len(), 2);
        assert!(registry.search("zzz").is_empty());
    }
}
