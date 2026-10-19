//! Language features from extensions, in front of the language servers.
//!
//! The editor asks one `LanguageServers`. This one asks the extensions first: when an extension
//! provides the feature for the file (a hover provider, a formatter, a code action provider...),
//! it answers; otherwise the real servers do. Problems from extensions' diagnostic collections
//! and from servers are put together per file, so neither wipes out the other.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::{
    Diagnostic, ExtensionHost, LanguageServers, LspRequest, LspResponse, ServerDefinition, ServerEvents,
};

/// The marker an extension's code action carries in its raw JSON, with `{command, arguments}`:
/// running it means running that command in the extension host.
pub const EXTENSION_COMMAND_KEY: &str = "rustcodeExtensionCommand";

#[derive(Default)]
struct Problems {
    /// What servers last reported for each file.
    servers: HashMap<PathBuf, Vec<Diagnostic>>,
    /// What each extension's collection last reported for each file.
    collections: HashMap<(String, PathBuf), Vec<Diagnostic>>,
}

impl Problems {
    /// Everything now known about `path`.
    fn all_for(&self, path: &Path) -> Vec<Diagnostic> {
        let mut all = self.servers.get(path).cloned().unwrap_or_default();
        let mut names: Vec<&(String, PathBuf)> = self.collections.keys().filter(|(_, p)| p == path).collect();
        names.sort();
        for key in names {
            all.extend(self.collections[key].iter().cloned());
        }
        all
    }
}

/// A question both an extension and a language server answer: the two answers are put together.
struct Merge {
    asked: std::time::Instant,
    /// The extension's answer, and the server's, when they are in.
    parts: [Option<LspResponse>; 2],
}

/// How long to wait for the second of two answers before giving the first.
const MERGE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Default)]
struct Merges {
    /// Questions waiting for answers, by the number the editor was given.
    open: HashMap<u64, Merge>,
    /// Which question (and which of its two parts: 0 extension, 1 server) an answer is for.
    parts: HashMap<u64, (u64, usize)>,
}

pub struct ExtensionLanguageServers {
    servers: Arc<dyn LanguageServers>,
    extensions: Arc<dyn ExtensionHost>,
    problems: Mutex<Problems>,
    merges: Mutex<Merges>,
    /// Answers made here (a command that was run), until `take_events` hands them over.
    local: Mutex<Vec<(u64, LspResponse)>>,
    next_local: Mutex<u64>,
    folders: Mutex<Vec<PathBuf>>,
}

/// Numbers for answers made here start high, so that they never match a number a server gave.
const LOCAL_BASE: u64 = 1 << 61;

/// Questions whose answers are lists that can be added together. (Formatting, renaming and the
/// signature of a call have one right answer: the extension's, when it provides one.)
fn is_mergeable(request: &LspRequest) -> bool {
    matches!(
        request,
        LspRequest::Hover { .. }
            | LspRequest::Definition { .. }
            | LspRequest::TypeDefinition { .. }
            | LspRequest::Implementation { .. }
            | LspRequest::References { .. }
            | LspRequest::Completion { .. }
            | LspRequest::CodeActions { .. }
            | LspRequest::FoldingRanges { .. }
            | LspRequest::InlayHints { .. }
            | LspRequest::DocumentLinks { .. }
    )
}

/// The two answers as one. A failed answer gives way to the other.
fn merged(extension: LspResponse, server: LspResponse) -> LspResponse {
    use LspResponse::*;
    match (extension, server) {
        (Failed(_), other) | (other, Failed(_)) => other,
        (Hover(a), Hover(b)) => Hover(match (a, b) {
            (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
            (a, b) => a.or(b),
        }),
        (Locations(mut a), Locations(b)) => {
            for place in b {
                if !a.contains(&place) {
                    a.push(place);
                }
            }
            Locations(a)
        }
        (Completions(mut a), Completions(b)) => {
            a.extend(b);
            Completions(a)
        }
        (CodeActions(mut a), CodeActions(b)) => {
            a.extend(b);
            CodeActions(a)
        }
        (Folds(mut a), Folds(b)) => {
            a.extend(b);
            Folds(a)
        }
        (Hints(mut a), Hints(b)) => {
            a.extend(b);
            Hints(a)
        }
        (Links(mut a), Links(b)) => {
            a.extend(b);
            Links(a)
        }
        (extension, _) => extension,
    }
}

impl ExtensionLanguageServers {
    pub fn new(servers: Arc<dyn LanguageServers>, extensions: Arc<dyn ExtensionHost>) -> Self {
        Self {
            servers,
            extensions,
            problems: Mutex::default(),
            merges: Mutex::default(),
            local: Mutex::default(),
            next_local: Mutex::new(LOCAL_BASE),
            folders: Mutex::default(),
        }
    }

    fn local_id(&self) -> Option<u64> {
        let mut next = self.next_local.lock().ok()?;
        let id = *next;
        *next += 1;
        Some(id)
    }

    /// Take the answers that are parts of a merged question out of `responses`, and add the merged
    /// answers (and those that waited too long) to it.
    fn merge_answers(&self, responses: &mut Vec<(u64, LspResponse)>) {
        let Ok(mut merges) = self.merges.lock() else { return };
        if merges.open.is_empty() {
            return;
        }
        let mut done: Vec<(u64, LspResponse)> = Vec::new();
        let mut rest = Vec::new();
        for (id, response) in responses.drain(..) {
            match merges.parts.get(&id).copied() {
                Some((question, part)) => {
                    if let Some(merge) = merges.open.get_mut(&question) {
                        merge.parts[part] = Some(response);
                    }
                }
                None => rest.push((id, response)),
            }
        }
        *responses = rest;
        let ready: Vec<u64> = merges
            .open
            .iter()
            .filter(|(_, m)| m.parts.iter().all(Option::is_some) || (m.asked.elapsed() > MERGE_WAIT && m.parts.iter().any(Option::is_some)))
            .map(|(id, _)| *id)
            .collect();
        for question in ready {
            if let Some(merge) = merges.open.remove(&question) {
                let [extension, server] = merge.parts;
                let answer = match (extension, server) {
                    (Some(e), Some(s)) => merged(e, s),
                    (Some(only), None) | (None, Some(only)) => only,
                    (None, None) => continue,
                };
                done.push((question, answer));
            }
            merges.parts.retain(|_, (q, _)| *q != question);
        }
        responses.extend(done);
    }
}

impl LanguageServers for ExtensionLanguageServers {
    fn sync(&self, folder: &Path, path: &Path, version: u64, text: &str) {
        if let Ok(mut folders) = self.folders.lock() {
            if !folders.iter().any(|f| f == folder) {
                folders.push(folder.to_path_buf());
            }
        }
        self.servers.sync(folder, path, version, text);
    }

    fn saved(&self, path: &Path) {
        self.servers.saved(path);
    }

    fn close(&self, path: &Path) {
        self.servers.close(path);
    }

    fn take_events(&self) -> ServerEvents {
        let mut events = self.servers.take_events();
        let from_extensions = self.extensions.take_language_events();
        events.responses.extend(from_extensions.responses);
        if let Ok(mut local) = self.local.lock() {
            events.responses.append(&mut local);
        }
        self.merge_answers(&mut events.responses);
        let Ok(mut problems) = self.problems.lock() else { return events };
        // Problems are replaced per file with everything known about it.
        let mut changed: Vec<PathBuf> = Vec::new();
        for (path, list) in std::mem::take(&mut events.diagnostics) {
            problems.servers.insert(path.clone(), list);
            changed.push(path);
        }
        for update in from_extensions.diagnostics {
            let key = (update.collection, update.path.clone());
            if update.items.is_empty() {
                problems.collections.remove(&key);
            } else {
                problems.collections.insert(key, update.items);
            }
            changed.push(update.path);
        }
        changed.sort();
        changed.dedup();
        events.diagnostics = changed.into_iter().map(|path| {
            let all = problems.all_for(&path);
            (path, all)
        }).collect();
        events
    }

    fn request(&self, request: LspRequest) -> Option<u64> {
        // A code action's command that belongs to an extension runs there.
        if let LspRequest::ExecuteCommand { raw, .. } = &request {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
                if let Some(command) = value.get(EXTENSION_COMMAND_KEY) {
                    let name = command["command"].as_str().unwrap_or_default();
                    let arguments: Vec<serde_json::Value> = command["arguments"].as_array().cloned().unwrap_or_default();
                    let folders = self.folders.lock().map(|f| f.clone()).unwrap_or_default();
                    self.extensions.run_command(name, &arguments, &folders);
                    let id = self.local_id()?;
                    self.local.lock().ok()?.push((id, LspResponse::Done));
                    return Some(id);
                }
            }
        }
        let mergeable = is_mergeable(&request);
        let Some(extension) = self.extensions.language_request(&request) else {
            return self.servers.request(request);
        };
        if !mergeable {
            return Some(extension);
        }
        // Both can answer: ask the server too, and give one answer when both are in.
        let Some(server) = self.servers.request(request) else { return Some(extension) };
        let question = self.local_id()?;
        let mut merges = self.merges.lock().ok()?;
        merges.open.insert(question, Merge { asked: std::time::Instant::now(), parts: [None, None] });
        merges.parts.insert(extension, (question, 0));
        merges.parts.insert(server, (question, 1));
        Some(question)
    }

    fn server_problem(&self, path: &Path) -> Option<String> {
        self.servers.server_problem(path)
    }

    fn retry_failed(&self) {
        self.servers.retry_failed();
    }

    fn set_auto_install(&self, on: bool) {
        self.servers.set_auto_install(on);
    }

    fn install_command(&self, path: &Path) -> Option<String> {
        self.servers.install_command(path)
    }

    fn configure(&self, user: Vec<ServerDefinition>) {
        self.servers.configure(user);
    }

    fn completion_triggers(&self, path: &Path) -> Vec<char> {
        let mut all = self.servers.completion_triggers(path);
        all.extend(self.extensions.completion_triggers(path));
        all.sort_unstable();
        all.dedup();
        all
    }

    fn format_triggers(&self, path: &Path) -> Vec<char> {
        let mut all = self.servers.format_triggers(path);
        all.extend(self.extensions.format_triggers(path));
        all.sort_unstable();
        all.dedup();
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CollectionDiagnostics, LanguageEvents, Severity};

    fn problem(path: &str, message: &str, source: &str) -> Diagnostic {
        Diagnostic {
            path: PathBuf::from(path),
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 1,
            severity: Severity::Warning,
            message: message.to_owned(),
            source: source.to_owned(),
        }
    }

    #[derive(Default)]
    struct FakeServers {
        events: Mutex<ServerEvents>,
        asked: Mutex<Vec<LspRequest>>,
    }

    impl LanguageServers for FakeServers {
        fn sync(&self, _: &Path, _: &Path, _: u64, _: &str) {}
        fn saved(&self, _: &Path) {}
        fn close(&self, _: &Path) {}
        fn take_events(&self) -> ServerEvents {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
        fn request(&self, request: LspRequest) -> Option<u64> {
            self.asked.lock().unwrap().push(request);
            Some(7)
        }
    }

    #[derive(Default)]
    struct FakeExtensions {
        events: Mutex<LanguageEvents>,
        /// Hover is provided; and formatting; nothing else.
        hover: bool,
        format: bool,
        commands: Mutex<Vec<(String, Vec<serde_json::Value>)>>,
    }

    impl ExtensionHost for FakeExtensions {
        fn commands(&self) -> Vec<crate::ExtensionCommand> {
            Vec::new()
        }
        fn execute(&self, _: &str, _: &[PathBuf]) {}
        fn take_events(&self) -> Vec<crate::HostEvent> {
            Vec::new()
        }
        fn language_request(&self, request: &LspRequest) -> Option<u64> {
            match request {
                LspRequest::Hover { .. } if self.hover => Some(1 << 40),
                LspRequest::Format { .. } if self.format => Some((1 << 40) + 1),
                _ => None,
            }
        }
        fn take_language_events(&self) -> LanguageEvents {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
        fn run_command(&self, command: &str, arguments: &[serde_json::Value], _: &[PathBuf]) {
            self.commands.lock().unwrap().push((command.to_owned(), arguments.to_vec()));
        }
    }

    fn joined(extensions: FakeExtensions) -> (ExtensionLanguageServers, Arc<FakeServers>, Arc<FakeExtensions>) {
        let servers = Arc::new(FakeServers::default());
        let extensions = Arc::new(extensions);
        (ExtensionLanguageServers::new(servers.clone(), extensions.clone()), servers, extensions)
    }

    #[test]
    fn a_feature_with_one_right_answer_goes_to_the_extension_and_the_servers_answer_the_rest() {
        let (both, servers, _) = joined(FakeExtensions { format: true, ..Default::default() });
        let at = |p: &str| PathBuf::from(p);
        assert_eq!(both.request(LspRequest::Format { path: at("/a.js"), tab_size: 2, insert_spaces: true }), Some((1 << 40) + 1));
        assert!(servers.asked.lock().unwrap().is_empty(), "the server was not asked");
        assert_eq!(both.request(LspRequest::Completion { path: at("/a.js"), line: 0, column: 0 }), Some(7));
        assert_eq!(servers.asked.lock().unwrap().len(), 1);
    }

    #[test]
    fn when_an_extension_and_a_server_both_answer_the_editor_gets_one_answer_with_both() {
        let (both, servers, extensions) = joined(FakeExtensions { hover: true, ..Default::default() });
        let id = both.request(LspRequest::Hover { path: PathBuf::from("/a.js"), line: 0, column: 0 }).unwrap();
        assert!(id >= LOCAL_BASE, "a number of its own");
        assert_eq!(servers.asked.lock().unwrap().len(), 1, "the server was asked as well");
        // One part in: nothing yet.
        extensions.events.lock().unwrap().responses.push((1 << 40, LspResponse::Hover(Some("from the extension".into()))));
        assert!(both.take_events().responses.is_empty());
        // The other arrives: one answer, under the number the editor was given.
        servers.events.lock().unwrap().responses.push((7, LspResponse::Hover(Some("from the server".into()))));
        assert_eq!(both.take_events().responses, vec![(id, LspResponse::Hover(Some("from the extension\n\nfrom the server".into())))]);
        assert!(both.take_events().responses.is_empty(), "given once");
    }

    #[test]
    fn a_part_that_failed_gives_way_and_places_are_not_listed_twice() {
        use crate::LspLocation;
        let place = |line| LspLocation { path: PathBuf::from("/a.js"), line, column: 0, end_line: line, end_column: 1 };
        assert_eq!(merged(LspResponse::Failed("no".into()), LspResponse::Hover(Some("x".into()))), LspResponse::Hover(Some("x".into())));
        assert_eq!(merged(LspResponse::Hover(None), LspResponse::Failed("no".into())), LspResponse::Hover(None));
        assert_eq!(merged(LspResponse::Hover(None), LspResponse::Hover(Some("y".into()))), LspResponse::Hover(Some("y".into())));
        assert_eq!(
            merged(LspResponse::Locations(vec![place(1), place(2)]), LspResponse::Locations(vec![place(2), place(3)])),
            LspResponse::Locations(vec![place(1), place(2), place(3)])
        );
        assert_eq!(merged(LspResponse::Edits(vec![]), LspResponse::Done), LspResponse::Edits(vec![]), "the extension's wins when they differ");
    }

    #[test]
    fn trigger_characters_come_from_both_and_are_listed_once() {
        struct Triggers;
        impl LanguageServers for Triggers {
            fn sync(&self, _: &Path, _: &Path, _: u64, _: &str) {}
            fn saved(&self, _: &Path) {}
            fn close(&self, _: &Path) {}
            fn take_events(&self) -> ServerEvents {
                ServerEvents::default()
            }
            fn completion_triggers(&self, _: &Path) -> Vec<char> {
                vec!['.', ':']
            }
        }
        struct Chars;
        impl ExtensionHost for Chars {
            fn commands(&self) -> Vec<crate::ExtensionCommand> {
                Vec::new()
            }
            fn execute(&self, _: &str, _: &[PathBuf]) {}
            fn take_events(&self) -> Vec<crate::HostEvent> {
                Vec::new()
            }
            fn completion_triggers(&self, _: &Path) -> Vec<char> {
                vec!['<', '.']
            }
        }
        let both = ExtensionLanguageServers::new(Arc::new(Triggers), Arc::new(Chars));
        assert_eq!(both.completion_triggers(Path::new("/a.js")), vec!['.', ':', '<']);
        assert!(both.format_triggers(Path::new("/a.js")).is_empty());
    }

    #[test]
    fn answers_from_extensions_come_with_those_from_servers() {
        let (both, servers, extensions) = joined(FakeExtensions::default());
        servers.events.lock().unwrap().responses.push((5, LspResponse::Done));
        extensions.events.lock().unwrap().responses.push((1 << 40, LspResponse::Hover(Some("x".into()))));
        let events = both.take_events();
        assert_eq!(events.responses, vec![(5, LspResponse::Done), (1 << 40, LspResponse::Hover(Some("x".into())))]);
    }

    #[test]
    fn problems_from_servers_and_collections_are_kept_apart_and_reported_together() {
        let (both, servers, extensions) = joined(FakeExtensions::default());
        let file = PathBuf::from("/a.js");
        servers.events.lock().unwrap().diagnostics.push((file.clone(), vec![problem("/a.js", "from the server", "ts")]));
        extensions.events.lock().unwrap().diagnostics.push(CollectionDiagnostics {
            collection: "eslint".into(),
            path: file.clone(),
            items: vec![problem("/a.js", "from eslint", "eslint")],
        });
        let events = both.take_events();
        assert_eq!(events.diagnostics.len(), 1);
        let messages: Vec<_> = events.diagnostics[0].1.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages, vec!["from the server", "from eslint"]);

        // The server clears its problems: the collection's stay.
        servers.events.lock().unwrap().diagnostics.push((file.clone(), vec![]));
        let events = both.take_events();
        assert_eq!(events.diagnostics[0].1.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(), vec!["from eslint"]);

        // The collection clears: nothing is left, and the file is reported empty so it is cleared.
        extensions.events.lock().unwrap().diagnostics.push(CollectionDiagnostics { collection: "eslint".into(), path: file.clone(), items: vec![] });
        let events = both.take_events();
        assert_eq!(events.diagnostics, vec![(file, vec![])]);
        // Nothing changed: nothing is said.
        assert!(both.take_events().diagnostics.is_empty());
    }

    #[test]
    fn the_command_of_an_extensions_code_action_runs_in_the_extension_host() {
        let (both, servers, extensions) = joined(FakeExtensions::default());
        let raw = serde_json::json!({EXTENSION_COMMAND_KEY: {"command": "eslint.fix", "arguments": [1, "two"]}}).to_string();
        let id = both.request(LspRequest::ExecuteCommand { path: PathBuf::from("/a.js"), raw }).expect("an answer number");
        assert!(id >= LOCAL_BASE);
        assert!(servers.asked.lock().unwrap().is_empty());
        assert_eq!(*extensions.commands.lock().unwrap(), vec![("eslint.fix".to_owned(), vec![serde_json::json!(1), serde_json::json!("two")])]);
        assert_eq!(both.take_events().responses, vec![(id, LspResponse::Done)]);
        // A server's own command goes to the server.
        let raw = serde_json::json!({"command": "x"}).to_string();
        assert_eq!(both.request(LspRequest::ExecuteCommand { path: PathBuf::from("/a.js"), raw }), Some(7));
    }
}
