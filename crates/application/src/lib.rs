//! Application layer: use cases and the ports (traits) infrastructure must implement.

pub mod commands;
pub mod workspace_trust;
mod debugger;
mod diagnostics;
pub mod explorer;
mod editorconfig;
mod emmet;
pub mod notebook;
mod notebook_kernel;
mod problem_matcher;
mod formatter;
mod contributed;
mod extension_host;
mod extension_language;
mod extension_services;
mod when_clause;
mod indentation;
mod snippets;
mod git;
mod icon_theme;
mod keybindings;
mod language_server;
mod linter;
pub mod ports;
pub mod replace;
pub mod session;
pub mod updates;
mod telemetry;
mod settings;
mod setting_catalog;
mod settings_edit;
mod incremental_highlighter;
pub mod terminal;
mod terminal_links;
mod shell_integration;
pub mod use_cases;
mod workspace_search;

pub use commands::{fuzzy_score, rank, Command, CommandId, CommandRegistry};
pub use ports::{
    DirEntry, FileError, FileLister, FileOperations, FileWatcher, WatchGuard, FileReader, FileWriter, HighlightSession, SyntaxHighlighter, ThemeLoadError,
    ThemeLoader, TokenizedLine,
};
pub use telemetry::{NoTelemetry, Telemetry};
pub use updates::{checksum_from_file, is_newer, package_matches, release_from_json, repository_slug, NoUpdates, Release, StagedUpdate, UpdateService};
pub use session::{PanelState, BreakpointState, GroupState, LayoutState, NullSessionStore, SessionState, SessionStore, TabState};
pub use explorer::{copy_name, ExplorerTree, TreeRow};
pub use incremental_highlighter::IncrementalHighlighter;
pub use use_cases::{LoadTheme, OpenDocument, SampleDocument, SaveDocument, SaveDocumentError};
pub use replace::{apply_edits, preview, replace_edits, ReplaceInFiles, ReplaceScope};
pub use workspace_search::{parse_globs, FileMatches, LineMatch, SearchQuery, SearchResults, TextSearcher};
pub use git::{
    parse_rebase_todo, BlameLine, ChangeKind, CommitInfo, CommitReport, ConflictSides, Credentials, GitChange,
    GitCommand, GitProvider, GitStatus, GraphCommit, PullRequest, RebaseStep, Signing, SubmoduleInfo, SyncAction,
};
pub use terminal_links::{find_links, link_at, word_range, TerminalLink};
pub use terminal::{PtyControl, PtyEvent, PtySession, ShellSpec, PtySpawner, TermCell, TermColor, TerminalEmulator, TerminalSnapshot};
pub use debugger::{
    parse_launch_json, parse_tasks_json, substitute, AdapterKind, BreakpointSpec, DebugAdapters, DebugCommand, DebugEvent, DebugRequest,
    EvalContext, LaunchConfig, LaunchRequest, NoDebugger, ProcessEntry, Scope, StackFrame, TaskSpec, Variable,
};
pub use diagnostics::{count_by_severity, Diagnostic, DiagnosticsProvider, Severity};
pub use contributed::{builtin_language_configuration, Contributed, GrammarSource, LanguageConfiguration, LanguageDefinition, SettingSchema};
pub use indentation::{
    is_valid_pattern, matches_pattern, outdent, plan_newline, should_outdent, EnterAction, IndentAction, IndentationRules,
    NewlinePlan, OnEnterRule,
};
pub use problem_matcher::{builtin_matcher, find_problems, matcher_from_value, matchers_from_value, plain_output, ProblemMatcher};
pub use notebook_kernel::{Kernel, KernelEvent, KernelLauncher, KernelOption, NoKernels, KernelVariable};
pub use emmet::{abbreviation_at_end, expand as expand_emmet, syntax_for_file as emmet_syntax_for_file, EmmetSyntax};
pub use snippets::{builtin_snippets, expand_snippet, parse_snippets, ExpandedSnippet, Mirror, Transform, plain_snippet_text, Snippet, SnippetLibrary};
pub use extension_language::ExtensionLanguageServers;
pub use when_clause::when_matches;
pub use shell_integration::{unescape as unescape_shell_text, MarkScanner, ShellEvent};
pub use extension_services::{
    CustomEditorInfo, ExtTerminalLink, USER_TERMINAL_BASE, WebviewOptions, WebviewSpec, ContributedView, ContributedWorkbench, MenuEntry, NodeIcon, ScmGroup, ScmResource, ScmSource, ServiceCommand, ServiceEvent, TaskInfo, TaskRun,
    TerminalRequest, TreeNode, ViewContainer,
};
pub use extension_host::{SaveReason, TreeUiEvent, 
    CollectionDiagnostics, DecoratedRange, DecorationEdge, DecorationStyle, ExtensionCommand, ExtensionHost, HostChange, HostDocument, HostEditor, HostEvent, HostFileEdit, HostPoint, HostSelection, InstalledPackage, MarketExtension, LanguageEvents, MessageLevel, NoExtensions, PickItem, PromptAnswer, PromptKind,
    SettingTarget, WorkbenchUpdate, untitled_path, untitled_title,
};
pub use editorconfig::{editorconfig_for, EditorConfig};
pub use formatter::{
    build_command, language_id, parse_default_formatter, BLACK, CLANG_FORMAT, ELM_FORMAT, GOFMT, PRETTIER, RUBOCOP, RUFF, RUSTFMT, SHFMT, STYLUA, SWIFTFORMAT, TAPLO, TERRAFORM, ZIG, CSHARPIER, DART, GLEAM, KTFMT, MIX, NIXFMT, OCAMLFORMAT, ORMOLU, GOOGLE_JAVA_FORMAT, SCALAFMT, plan as plan_formatting, run_programs, rust_edition, optional_tools, usual_tools, FormatCommand,
    FormatFailure, FormatOptions, FormatPlan, FormatRunner, FormatterChoice, NoFormatRunner,
};
pub use linter::{
    build_command as build_lint_command, linters_for, parse_output as parse_lint_output, plan as plan_linting, run_fix_all, run_lint, rust_check_command,
    LintCommand, LintFailure, LintFinding, LintFix, LintOutput, LintRunner, LintScope, NoLintRunner, PlannedLint, StdinForm, CLIPPY,
};
pub use icon_theme::IconTheme;
pub use keybindings::{
    eval_when, parse_builtin_keys, parse_chord, parse_keybindings, parse_keys, Chord, ContextValue, UserBinding,
};
pub use setting_catalog::{categories as setting_categories, find_setting, title_of as setting_title, SettingDef, SettingKind, SETTINGS};
pub use settings_edit::{append_array_item, remove_array_items, remove_setting, set_setting, set_string_list_setting, set_string_setting};
pub use settings::{line_height_for, BracketGuides, LintRun, match_theme_name, EditorSettings, Settings, DEFAULT_LINE_HEIGHT_RATIO};
pub use language_server::{
    CallEntry, CodeActionEntry, CompletionEntry, FileEdit, InlayHintEntry, LinkEntry, LanguageServers, ColorEntry, ColorPresentationEntry, InlineEntry, LensEntry, LspLocation, LspRequest, LspResponse, NoLanguageServers,
    SemanticToken, ServerDefinition, ServerEvents, SignatureInfo, SymbolEntry, TextEdit, parse_server_definitions,
};
