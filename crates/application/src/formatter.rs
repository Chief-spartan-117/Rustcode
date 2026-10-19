//! Formatting with programs: which formatter a file gets, the command line that runs it, and
//! the use case that runs the candidates. The programs themselves are started by infrastructure
//! behind [`FormatRunner`]. Language servers are the fallback (see `LspRequest::Format`).

use std::path::{Path, PathBuf};

use crate::editorconfig::EditorConfig;
use crate::ports::FileReader;

pub const RUSTFMT: &str = "rustfmt";
pub const PRETTIER: &str = "prettier";
pub const RUFF: &str = "ruff";
pub const BLACK: &str = "black";
pub const GOFMT: &str = "gofmt";
pub const CLANG_FORMAT: &str = "clang-format";
pub const SHFMT: &str = "shfmt";
pub const TAPLO: &str = "taplo";
pub const STYLUA: &str = "stylua";
pub const ZIG: &str = "zig";
pub const GOOGLE_JAVA_FORMAT: &str = "google-java-format";
pub const TERRAFORM: &str = "terraform";
pub const SWIFTFORMAT: &str = "swiftformat";
pub const KTFMT: &str = "ktfmt";
pub const RUBOCOP: &str = "rubocop";
pub const ELM_FORMAT: &str = "elm-format";
pub const NIXFMT: &str = "nixfmt";
pub const MIX: &str = "mix";
pub const SCALAFMT: &str = "scalafmt";
pub const OCAMLFORMAT: &str = "ocamlformat";
pub const GLEAM: &str = "gleam";
pub const DART: &str = "dart";
pub const ORMOLU: &str = "ormolu";
pub const CSHARPIER: &str = "csharpier";

/// VS Code's language id for a file, from its name. These are the ids of `"[rust]": {...}` blocks.
pub fn language_id(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name == "dockerfile" {
        return Some("dockerfile");
    }
    let (_, ext) = name.rsplit_once('.')?;
    Some(match ext {
        "rs" => "rust",
        "go" => "go",
        "py" | "pyi" => "python",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "m" => "objective-c",
        "mm" => "objective-cpp",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "json" => "json",
        "jsonc" => "jsonc",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "html" | "htm" => "html",
        "vue" => "vue",
        "md" | "markdown" => "markdown",
        "yml" | "yaml" => "yaml",
        "sh" | "bash" | "zsh" => "shellscript",
        "toml" => "toml",
        "java" => "java",
        "cs" => "csharp",
        "php" => "php",
        "rb" => "ruby",
        "swift" => "swift",
        "kt" => "kotlin",
        "lua" => "lua",
        "sql" => "sql",
        "xml" => "xml",
        "zig" => "zig",
        "tf" | "tfvars" => "terraform",
        "kts" => "kotlin",
        "elm" => "elm",
        "nix" => "nix",
        "ex" | "exs" => "elixir",
        "scala" | "sc" => "scala",
        "ml" | "mli" => "ocaml",
        "gleam" => "gleam",
        "dart" => "dart",
        "hs" => "haskell",
        _ => return None,
    })
}

/// What `editor.defaultFormatter` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatterChoice {
    /// Not set: the program usual for the language, else the language server.
    Auto,
    /// A program formatter, by name.
    Tool(&'static str),
    /// Something this editor has no program for: the language server formats.
    LanguageServer,
}

/// Read `editor.defaultFormatter`: an extension id such as `esbenp.prettier-vscode` or
/// `rust-lang.rust-analyzer`, or a plain tool name.
pub fn parse_default_formatter(setting: Option<&str>) -> FormatterChoice {
    let id = setting.map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();
    if id.is_empty() {
        return FormatterChoice::Auto;
    }
    const KNOWN: &[(&str, &str)] = &[
        ("prettier", PRETTIER),
        ("rustfmt", RUSTFMT),
        ("rust-analyzer", RUSTFMT),
        ("ruff", RUFF),
        ("black", BLACK),
        ("gofmt", GOFMT),
        ("golang.go", GOFMT),
        ("clang-format", CLANG_FORMAT),
        ("clangd", CLANG_FORMAT),
        ("cpptools", CLANG_FORMAT),
        ("shfmt", SHFMT),
        ("shell-format", SHFMT),
        ("taplo", TAPLO),
        ("even-better-toml", TAPLO),
        ("stylua", STYLUA),
        ("zig", ZIG),
        ("google-java-format", GOOGLE_JAVA_FORMAT),
        ("terraform", TERRAFORM),
        ("swiftformat", SWIFTFORMAT),
        ("nicklockwood", SWIFTFORMAT),
        ("hashicorp", TERRAFORM),
        ("sumneko", STYLUA),
        ("ktfmt", KTFMT),
        ("rubocop", RUBOCOP),
        ("shopify.ruby-lsp", RUBOCOP),
        ("elm-format", ELM_FORMAT),
        ("elm-tooling", ELM_FORMAT),
        ("nixfmt", NIXFMT),
        ("mix", MIX),
        ("elixir-ls", MIX),
        ("scalafmt", SCALAFMT),
        ("scalameta", SCALAFMT),
        ("ocamlformat", OCAMLFORMAT),
        ("ocamllabs", OCAMLFORMAT),
        ("gleam", GLEAM),
        ("dart-code", DART),
        ("ormolu", ORMOLU),
        ("haskell.haskell", ORMOLU),
        ("csharpier", CSHARPIER),
    ];
    KNOWN
        .iter()
        .find(|(part, _)| id.contains(part))
        .map_or(FormatterChoice::LanguageServer, |(_, tool)| FormatterChoice::Tool(tool))
}

/// The programs to try for a language when none is chosen, best first.
pub fn usual_tools(language: &str) -> &'static [&'static str] {
    match language {
        "kotlin" => &[KTFMT],
        "dart" => &[DART],
        "haskell" => &[ORMOLU],
        "csharp" => &[CSHARPIER],
        "ruby" => &[RUBOCOP],
        "elm" => &[ELM_FORMAT],
        "nix" => &[NIXFMT],
        "elixir" => &[MIX],
        "scala" => &[SCALAFMT],
        "ocaml" => &[OCAMLFORMAT],
        "gleam" => &[GLEAM],
        "toml" => &[TAPLO],
        "lua" => &[STYLUA],
        "zig" => &[ZIG],
        "java" => &[GOOGLE_JAVA_FORMAT],
        "terraform" => &[TERRAFORM],
        "swift" => &[SWIFTFORMAT],
        "rust" => &[RUSTFMT],
        "go" => &[GOFMT],
        "python" => &[RUFF, BLACK],
        "c" | "cpp" | "objective-c" | "objective-cpp" => &[CLANG_FORMAT],
        "shellscript" => &[SHFMT],
        "javascript" | "javascriptreact" | "typescript" | "typescriptreact" | "json" | "jsonc" | "css" | "scss" | "less"
        | "html" | "vue" | "markdown" | "yaml" => &[PRETTIER],
        _ => &[],
    }
}

/// Programs offered for a language by "Format Document With..." and `editor.defaultFormatter`, but
/// never chosen by themselves: they do more than format. None at present.
pub fn optional_tools(_language: &str) -> &'static [&'static str] {
    &[]
}

/// Settings that shape a formatter's run.
#[derive(Debug, Clone, Default)]
pub struct FormatOptions {
    pub tab_size: usize,
    pub insert_spaces: bool,
    pub editorconfig: EditorConfig,
}

/// A program to run, with the file's text on its standard input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatCommand {
    pub tool: &'static str,
    pub program: String,
    /// Other names the program goes by (`dotnet-csharpier` for `csharpier`), tried if `program`
    /// is not found.
    pub alt_programs: Vec<String>,
    pub args: Vec<String>,
    /// Where a project keeps its own copy, relative to a folder above the file
    /// (`node_modules/.bin/prettier`); it wins over the one on `PATH`.
    pub local_bin: Option<String>,
    /// The folder to run in: the file's, or the project's for tools that only look there.
    pub cwd: PathBuf,
    /// Exit codes that still mean "formatted" (`rubocop` exits 1 when offenses are left).
    pub accept_codes: Vec<i32>,
    /// A line the tool may print before the formatted text: when the output has it, it and what
    /// comes before go.
    pub output_after: Option<String>,
}

/// Whether `dir` or a folder above it holds a Prettier config, or a `package.json` that names it.
fn prettier_configured(reader: &dyn FileReader, path: &Path) -> bool {
    const CONFIGS: [&str; 13] = [
        ".prettierrc",
        ".prettierrc.json",
        ".prettierrc.json5",
        ".prettierrc.yaml",
        ".prettierrc.yml",
        ".prettierrc.toml",
        ".prettierrc.js",
        ".prettierrc.cjs",
        ".prettierrc.mjs",
        "prettier.config.js",
        "prettier.config.cjs",
        "prettier.config.mjs",
        "prettier.config.ts",
    ];
    let mut dir = path.parent();
    while let Some(folder) = dir {
        if let Ok(entries) = reader.list_dir(folder) {
            if entries.iter().any(|e| !e.is_dir && CONFIGS.contains(&e.name.as_str())) {
                return true;
            }
        }
        if let Ok(package) = reader.read_to_string(&folder.join("package.json")) {
            if package.contains("\"prettier\"") {
                return true;
            }
        }
        dir = folder.parent();
    }
    false
}

/// The `edition` of the Rust project holding `path`, as Cargo would find it (a member may take it
/// from the workspace). 2021 when nothing says.
pub fn rust_edition(reader: &dyn FileReader, path: &Path) -> String {
    /// The edition under `[section]` of a manifest; `Some(None)` when it comes from the workspace.
    fn edition_in(text: &str, section: &str) -> Option<Option<String>> {
        let mut current = "";
        for line in text.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = name.trim();
                continue;
            }
            if current != section {
                continue;
            }
            if let Some(rest) = line.strip_prefix("edition") {
                if rest.trim_start().starts_with('=') && rest.contains('"') {
                    let value = rest.split('"').nth(1)?.to_owned();
                    return Some(Some(value));
                }
                if rest.starts_with(".workspace") || rest.contains("workspace") {
                    return Some(None);
                }
            }
        }
        None
    }
    let mut inherit = false;
    let mut dir = path.parent();
    while let Some(folder) = dir {
        if let Ok(text) = reader.read_to_string(&folder.join("Cargo.toml")) {
            if !inherit {
                match edition_in(&text, "package") {
                    Some(Some(edition)) => return edition,
                    Some(None) => inherit = true,
                    None if text.lines().any(|l| l.trim() == "[package]") => break,
                    None => {}
                }
            }
            if inherit || text.lines().any(|l| l.trim() == "[workspace]") {
                if let Some(Some(edition)) = edition_in(&text, "workspace.package") {
                    return edition;
                }
            }
        }
        dir = folder.parent();
    }
    "2021".to_owned()
}

/// Whether a `rustfmt.toml` or `.rustfmt.toml` is in `path`'s folder or above.
fn rustfmt_configured(reader: &dyn FileReader, path: &Path) -> bool {
    let mut dir = path.parent();
    while let Some(folder) = dir {
        if [".rustfmt.toml", "rustfmt.toml"].iter().any(|name| reader.read_to_string(&folder.join(name)).is_ok()) {
            return true;
        }
        dir = folder.parent();
    }
    false
}

/// The nearest folder above `path` that holds one of `names`.
fn folder_with(reader: &dyn FileReader, path: &Path, names: &[&str]) -> Option<PathBuf> {
    let mut dir = path.parent();
    while let Some(folder) = dir {
        if reader.list_dir(folder).is_ok_and(|entries| entries.iter().any(|e| names.contains(&e.name.as_str()))) {
            return Some(folder.to_path_buf());
        }
        dir = folder.parent();
    }
    None
}

/// The command that formats `path` with `tool`. The text goes to its standard input and the
/// formatted text comes from its standard output.
pub fn build_command(tool: &'static str, path: &Path, reader: &dyn FileReader, options: &FormatOptions) -> FormatCommand {
    let file = path.to_string_lossy().into_owned();
    let mut cwd = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut accept_codes = vec![0];
    let mut alt_programs: Vec<String> = Vec::new();
    let mut output_after = None;
    let (program, args, local_bin): (&str, Vec<String>, Option<&str>) = match tool {
        RUSTFMT => {
            let mut args = vec!["--emit".to_owned(), "stdout".to_owned(), "--edition".to_owned(), rust_edition(reader, path)];
            // `.editorconfig` speaks for a project that has no rustfmt config of its own.
            let config = &options.editorconfig;
            if let (true, Some(width)) = (!rustfmt_configured(reader, path), config.indent_width()) {
                args.push("--config".to_owned());
                args.push(format!("hard_tabs={},tab_spaces={}", config.spaces == Some(false), width));
            }
            (RUSTFMT, args, None)
        }
        PRETTIER => (PRETTIER, vec!["--stdin-filepath".to_owned(), file], Some("node_modules/.bin/prettier")),
        RUFF => (RUFF, vec!["format".to_owned(), "--stdin-filename".to_owned(), file, "-".to_owned()], None),
        BLACK => (BLACK, vec!["-q".to_owned(), "--stdin-filename".to_owned(), file, "-".to_owned()], None),
        CLANG_FORMAT => (CLANG_FORMAT, vec![format!("--assume-filename={file}")], None),
        SHFMT => {
            let mut args = vec!["-filename".to_owned(), file];
            // shfmt reads `.editorconfig` itself; without one, the editor's indentation applies.
            if !options.editorconfig.has_indentation() {
                args.push("-i".to_owned());
                args.push(if options.insert_spaces { options.tab_size.to_string() } else { "0".to_owned() });
            }
            (SHFMT, args, None)
        }
        TAPLO => (TAPLO, vec!["format".to_owned(), "--stdin-filepath".to_owned(), file, "-".to_owned()], None),
        STYLUA => (STYLUA, vec!["--stdin-filepath".to_owned(), file, "-".to_owned()], None),
        ZIG => (ZIG, vec!["fmt".to_owned(), "--stdin".to_owned()], None),
        GOOGLE_JAVA_FORMAT => (GOOGLE_JAVA_FORMAT, vec!["-".to_owned()], None),
        TERRAFORM => (TERRAFORM, vec!["fmt".to_owned(), "-".to_owned()], None),
        SWIFTFORMAT => (SWIFTFORMAT, vec!["--stdinpath".to_owned(), file], None),
        KTFMT => (KTFMT, vec!["-".to_owned()], None),
        RUBOCOP => {
            // Exit 1 is "offenses are left", not a failure. Only its formatting cops run: its other
            // corrections change the code (it deletes an assignment nothing reads).
            accept_codes = vec![0, 1];
            output_after = Some("====================".to_owned());
            let args = ["-a", "--only", "Layout", "--stdin", &file, "--stderr", "--format", "quiet"];
            (RUBOCOP, args.iter().map(|a| (*a).to_owned()).collect(), None)
        }
        ELM_FORMAT => (ELM_FORMAT, vec!["--stdin".to_owned()], None),
        NIXFMT => (NIXFMT, Vec::new(), None),
        MIX => {
            // `mix` only looks in the folder it runs in.
            cwd = folder_with(reader, path, &["mix.exs"]).unwrap_or(cwd);
            (MIX, vec!["format".to_owned(), "-".to_owned()], None)
        }
        SCALAFMT => {
            cwd = folder_with(reader, path, &[".scalafmt.conf"]).unwrap_or(cwd);
            (SCALAFMT, vec!["--stdin".to_owned()], None)
        }
        // Without the flag it leaves a file outside a project (no `.ocamlformat` above it) as it is.
        OCAMLFORMAT => (
            OCAMLFORMAT,
            ["--enable-outside-detected-project", "--name", &file, "-"].iter().map(|a| (*a).to_owned()).collect(),
            None,
        ),
        GLEAM => (GLEAM, vec!["format".to_owned(), "--stdin".to_owned()], None),
        // With no file to format, `dart format` reads the text from standard input.
        DART => (DART, vec!["format".to_owned()], None),
        ORMOLU => (ORMOLU, vec!["--stdin-input-file".to_owned(), file], None),
        CSHARPIER => {
            alt_programs = vec!["dotnet-csharpier".to_owned()];
            (CSHARPIER, ["format", "--write-stdout", "--stdin-path", &file].iter().map(|a| (*a).to_owned()).collect(), None)
        }
        _ => (GOFMT, Vec::new(), None),
    };
    FormatCommand { tool, program: program.to_owned(), alt_programs, args, local_bin: local_bin.map(str::to_owned), cwd, accept_codes, output_after }
}

/// How to format one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatPlan {
    /// Programs to try in order. `explicit`: the user chose it, so "not installed" is an error;
    /// otherwise the language server formats when none of them is there.
    Programs { commands: Vec<FormatCommand>, explicit: bool },
    LanguageServer,
}

pub fn plan(
    reader: &dyn FileReader,
    path: &Path,
    choice: FormatterChoice,
    options: &FormatOptions,
) -> FormatPlan {
    match choice {
        FormatterChoice::LanguageServer => FormatPlan::LanguageServer,
        FormatterChoice::Tool(tool) => {
            FormatPlan::Programs { commands: vec![build_command(tool, path, reader, options)], explicit: true }
        }
        FormatterChoice::Auto => {
            let tools = language_id(path).map_or(&[][..], usual_tools);
            let commands: Vec<FormatCommand> = tools
                .iter()
                // Prettier is nobody's default: the project has to ask for it.
                .filter(|tool| **tool != PRETTIER || prettier_configured(reader, path))
                .map(|tool| build_command(tool, path, reader, options))
                .collect();
            if commands.is_empty() {
                FormatPlan::LanguageServer
            } else {
                FormatPlan::Programs { commands, explicit: false }
            }
        }
    }
}

/// Why a program did not format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatFailure {
    /// The program is not on the machine.
    NotInstalled,
    /// It ran and said no: the file does not parse, or the config is wrong.
    Failed(String),
}

/// Port: run a formatter program. Implemented by infrastructure.
pub trait FormatRunner: Send + Sync {
    /// Run `command` with `input` on its standard input; the output is the formatted text.
    fn run(&self, command: &FormatCommand, input: &str) -> Result<String, FormatFailure>;

    /// Whether the program of `command` is on the machine.
    fn available(&self, _command: &FormatCommand) -> bool {
        true
    }
}

/// A runner that finds no programs.
pub struct NoFormatRunner;

impl FormatRunner for NoFormatRunner {
    fn run(&self, _: &FormatCommand, _: &str) -> Result<String, FormatFailure> {
        Err(FormatFailure::NotInstalled)
    }

    fn available(&self, _: &FormatCommand) -> bool {
        false
    }
}

/// Format `input` with the first of `commands` that is installed. `Ok(None)`: none is installed
/// (and the choice was not explicit). `Err` carries a message for the user.
pub fn run_programs(
    runner: &dyn FormatRunner,
    commands: &[FormatCommand],
    input: &str,
    explicit: bool,
) -> Result<Option<(&'static str, String)>, String> {
    for command in commands {
        match runner.run(command, input) {
            Ok(text) => {
                let text = match &command.output_after {
                    Some(marker) => match text.split_once(&format!("{marker}\n")).or_else(|| text.split_once(&format!("{marker}\r\n"))) {
                        Some((_, formatted)) => formatted.to_owned(),
                        None => text,
                    },
                    None => text,
                };
                if text.is_empty() && !input.trim().is_empty() {
                    return Err(format!("{} returned nothing, so the file was left alone", command.tool));
                }
                return Ok(Some((command.tool, text)));
            }
            Err(FormatFailure::NotInstalled) => {}
            Err(FormatFailure::Failed(why)) => return Err(format!("{}: {why}", command.tool)),
        }
    }
    match (explicit, commands.first()) {
        (true, Some(first)) => Err(format!("{} is not installed", first.tool)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::ports::{DirEntry, FileError};

    #[derive(Default)]
    struct Files(HashMap<&'static str, &'static str>);

    impl FileReader for Files {
        fn read_to_string(&self, path: &Path) -> Result<String, FileError> {
            self.0
                .get(path.to_str().unwrap())
                .map(|s| (*s).to_owned())
                .ok_or(FileError::Io { path: path.display().to_string(), reason: "none".into() })
        }
        fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FileError> {
            let prefix = format!("{}/", path.to_str().unwrap().trim_end_matches('/'));
            Ok(self
                .0
                .keys()
                .filter_map(|k| k.strip_prefix(&prefix))
                .filter(|rest| !rest.contains('/'))
                .map(|name| DirEntry { name: name.to_owned(), is_dir: false })
                .collect())
        }
    }

    fn files(list: &[(&'static str, &'static str)]) -> Files {
        Files(list.iter().copied().collect())
    }

    fn opts() -> FormatOptions {
        FormatOptions { tab_size: 4, insert_spaces: true, editorconfig: EditorConfig::default() }
    }

    #[test]
    fn languages_by_extension() {
        let id = |p: &str| language_id(Path::new(p));
        assert_eq!(id("/a/main.rs"), Some("rust"));
        assert_eq!(id("x.TSX"), Some("typescriptreact"));
        assert_eq!(id("script.sh"), Some("shellscript"));
        assert_eq!(id("Dockerfile"), Some("dockerfile"));
        assert_eq!(id("notes"), None);
        assert_eq!(id("a.unknown"), None);
    }

    #[test]
    fn the_default_formatter_setting_names_a_tool_or_the_server() {
        use FormatterChoice::*;
        assert_eq!(parse_default_formatter(None), Auto);
        assert_eq!(parse_default_formatter(Some("")), Auto);
        assert_eq!(parse_default_formatter(Some("esbenp.prettier-vscode")), Tool(PRETTIER));
        assert_eq!(parse_default_formatter(Some("rust-lang.rust-analyzer")), Tool(RUSTFMT));
        assert_eq!(parse_default_formatter(Some("ms-python.black-formatter")), Tool(BLACK));
        assert_eq!(parse_default_formatter(Some("charliermarsh.ruff")), Tool(RUFF));
        assert_eq!(parse_default_formatter(Some("golang.go")), Tool(GOFMT));
        assert_eq!(parse_default_formatter(Some("vscode.typescript-language-features")), LanguageServer);
    }

    #[test]
    fn commands_carry_the_file_and_the_tool_specific_flags() {
        let reader = files(&[]);
        let path = Path::new("/p/src/lib.rs");
        let rustfmt = build_command(RUSTFMT, path, &reader, &opts());
        assert_eq!(rustfmt.program, "rustfmt");
        assert_eq!(rustfmt.args, ["--emit", "stdout", "--edition", "2021"]);
        assert_eq!(rustfmt.cwd, Path::new("/p/src"));
        let prettier = build_command(PRETTIER, Path::new("/p/a.ts"), &reader, &opts());
        assert_eq!(prettier.args, ["--stdin-filepath", "/p/a.ts"]);
        assert_eq!(prettier.local_bin.as_deref(), Some("node_modules/.bin/prettier"));
        assert_eq!(build_command(RUFF, Path::new("/p/a.py"), &reader, &opts()).args, ["format", "--stdin-filename", "/p/a.py", "-"]);
        assert_eq!(build_command(BLACK, Path::new("/p/a.py"), &reader, &opts()).args, ["-q", "--stdin-filename", "/p/a.py", "-"]);
        assert_eq!(build_command(CLANG_FORMAT, Path::new("/p/a.c"), &reader, &opts()).args, ["--assume-filename=/p/a.c"]);
        assert!(build_command(GOFMT, Path::new("/p/a.go"), &reader, &opts()).args.is_empty());
    }

    #[test]
    fn the_other_tools_get_their_command_lines() {
        let reader = files(&[]);
        let args = |tool: &'static str, file: &str| build_command(tool, Path::new(file), &reader, &opts()).args;
        assert_eq!(args(TAPLO, "/p/a.toml"), ["format", "--stdin-filepath", "/p/a.toml", "-"]);
        assert_eq!(args(STYLUA, "/p/a.lua"), ["--stdin-filepath", "/p/a.lua", "-"]);
        assert_eq!(args(ZIG, "/p/a.zig"), ["fmt", "--stdin"]);
        assert_eq!(args(GOOGLE_JAVA_FORMAT, "/p/A.java"), ["-"]);
        assert_eq!(args(TERRAFORM, "/p/a.tf"), ["fmt", "-"]);
        assert_eq!(args(SWIFTFORMAT, "/p/a.swift"), ["--stdinpath", "/p/a.swift"]);
        assert_eq!(build_command(TERRAFORM, Path::new("/p/a.tf"), &reader, &opts()).program, "terraform");
        // The usual tool of each of those languages.
        for (file, tool) in [("a.toml", TAPLO), ("a.lua", STYLUA), ("a.zig", ZIG), ("A.java", GOOGLE_JAVA_FORMAT), ("a.tfvars", TERRAFORM), ("a.swift", SWIFTFORMAT)] {
            match plan(&reader, &Path::new("/p").join(file), FormatterChoice::Auto, &opts()) {
                FormatPlan::Programs { commands, .. } => assert_eq!(commands[0].tool, tool, "{file}"),
                other => panic!("{file}: {other:?}"),
            }
        }
        assert_eq!(parse_default_formatter(Some("tamasfe.even-better-toml")), FormatterChoice::Tool(TAPLO));
        assert_eq!(parse_default_formatter(Some("JohnnyMorganz.stylua")), FormatterChoice::Tool(STYLUA));
    }

    #[test]
    fn dart_haskell_and_csharp_get_their_command_lines() {
        let reader = files(&[]);
        let command = |tool: &'static str, file: &str| build_command(tool, Path::new(file), &reader, &opts());
        assert_eq!(command(DART, "/p/a.dart").args, ["format"]);
        assert_eq!(command(ORMOLU, "/p/A.hs").args, ["--stdin-input-file", "/p/A.hs"]);
        let csharpier = command(CSHARPIER, "/p/A.cs");
        assert_eq!(csharpier.args, ["format", "--write-stdout", "--stdin-path", "/p/A.cs"]);
        assert_eq!((csharpier.program.as_str(), csharpier.alt_programs.as_slice()), ("csharpier", ["dotnet-csharpier".to_owned()].as_slice()));
        assert!(command(DART, "/p/a.dart").alt_programs.is_empty());
        for (file, tool) in [("a.dart", DART), ("A.hs", ORMOLU), ("A.cs", CSHARPIER)] {
            match plan(&reader, &Path::new("/p").join(file), FormatterChoice::Auto, &opts()) {
                FormatPlan::Programs { commands, .. } => assert_eq!(commands[0].tool, tool, "{file}"),
                other => panic!("{file}: {other:?}"),
            }
        }
        assert_eq!(parse_default_formatter(Some("csharpier.csharpier-vscode")), FormatterChoice::Tool(CSHARPIER));
        assert_eq!(parse_default_formatter(Some("Dart-Code.dart-code")), FormatterChoice::Tool(DART));
        assert_eq!(parse_default_formatter(Some("haskell.haskell")), FormatterChoice::Tool(ORMOLU));
    }

    #[test]
    fn the_later_tools_get_their_command_lines_and_folders() {
        let reader = files(&[("/proj/mix.exs", ""), ("/proj/lib/a.ex", ""), ("/s/.scalafmt.conf", "")]);
        let command = |tool: &'static str, file: &str| build_command(tool, Path::new(file), &reader, &opts());
        assert_eq!(command(KTFMT, "/p/A.kt").args, ["-"]);
        assert_eq!(command(ELM_FORMAT, "/p/A.elm").args, ["--stdin"]);
        assert!(command(NIXFMT, "/p/a.nix").args.is_empty());
        assert_eq!(command(OCAMLFORMAT, "/p/a.ml").args, ["--enable-outside-detected-project", "--name", "/p/a.ml", "-"]);
        assert_eq!(command(GLEAM, "/p/a.gleam").args, ["format", "--stdin"]);
        // `mix` and `scalafmt` run where their project file is.
        let mix = command(MIX, "/proj/lib/a.ex");
        assert_eq!((mix.args.as_slice(), mix.cwd.as_path()), (["format".to_owned(), "-".to_owned()].as_slice(), Path::new("/proj")));
        assert_eq!(command(SCALAFMT, "/s/src/A.scala").cwd, Path::new("/s"));
        assert_eq!(command(SCALAFMT, "/other/A.scala").cwd, Path::new("/other"));
        let rubocop = command(RUBOCOP, "/p/a.rb");
        assert_eq!(rubocop.accept_codes, [0, 1]);
        assert_eq!(rubocop.output_after.as_deref(), Some("===================="));
        assert_eq!(parse_default_formatter(Some("rubocop")), FormatterChoice::Tool(RUBOCOP));
        assert!(rubocop.args.windows(2).any(|w| w == ["--only", "Layout"]));
        assert!(optional_tools("ruby").is_empty());
        for (file, tool) in [("a.rb", RUBOCOP), ("A.kts", KTFMT), ("a.elm", ELM_FORMAT), ("a.nix", NIXFMT), ("a.exs", MIX), ("A.scala", SCALAFMT), ("a.mli", OCAMLFORMAT), ("a.gleam", GLEAM)] {
            match plan(&reader, &Path::new("/p").join(file), FormatterChoice::Auto, &opts()) {
                FormatPlan::Programs { commands, .. } => assert_eq!(commands[0].tool, tool, "{file}"),
                other => panic!("{file}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_banner_before_the_formatted_text_is_dropped_when_there_is_one() {
        let runner = Scripted(Mutex::new(vec![Ok("====================\nputs 1\n".into()), Ok("puts 2\n".into())]));
        let command = [build_command(RUBOCOP, Path::new("/p/a.rb"), &files(&[]), &opts())];
        assert_eq!(run_programs(&runner, &command, "puts  1", true), Ok(Some((RUBOCOP, "puts 1\n".to_owned()))));
        // Without the banner (as rubocop prints it with `--stderr`) the output is the text.
        assert_eq!(run_programs(&runner, &command, "puts  2", true), Ok(Some((RUBOCOP, "puts 2\n".to_owned()))));
    }

    #[test]
    fn rustfmt_takes_the_edition_from_the_manifest_or_the_workspace() {
        let own = files(&[("/p/Cargo.toml", "[package]\nname = \"x\"\nedition = \"2018\"\n")]);
        assert_eq!(rust_edition(&own, Path::new("/p/src/lib.rs")), "2018");
        let member = files(&[
            ("/w/Cargo.toml", "[workspace]\nmembers = [\"a\"]\n[workspace.package]\nedition = \"2024\"\n"),
            ("/w/a/Cargo.toml", "[package]\nname = \"a\"\nedition.workspace = true\n"),
        ]);
        assert_eq!(rust_edition(&member, Path::new("/w/a/src/lib.rs")), "2024");
        assert_eq!(rust_edition(&files(&[]), Path::new("/q/x.rs")), "2021");
    }

    #[test]
    fn editorconfig_reaches_rustfmt_unless_the_project_has_a_rustfmt_config() {
        let mut options = opts();
        options.editorconfig = EditorConfig { spaces: Some(false), tab_width: Some(8), ..Default::default() };
        let plain = build_command(RUSTFMT, Path::new("/p/a.rs"), &files(&[]), &options);
        assert_eq!(&plain.args[4..], ["--config", "hard_tabs=true,tab_spaces=8"]);
        let configured = build_command(RUSTFMT, Path::new("/p/a.rs"), &files(&[("/p/rustfmt.toml", "")]), &options);
        assert_eq!(configured.args.len(), 4);
    }

    #[test]
    fn shfmt_gets_the_editors_indentation_unless_editorconfig_speaks() {
        let reader = files(&[]);
        let tabs = FormatOptions { insert_spaces: false, ..opts() };
        assert_eq!(build_command(SHFMT, Path::new("/a.sh"), &reader, &opts()).args, ["-filename", "/a.sh", "-i", "4"]);
        assert_eq!(build_command(SHFMT, Path::new("/a.sh"), &reader, &tabs).args, ["-filename", "/a.sh", "-i", "0"]);
        let with_config = FormatOptions { editorconfig: EditorConfig { indent_size: Some(2), ..Default::default() }, ..opts() };
        assert_eq!(build_command(SHFMT, Path::new("/a.sh"), &reader, &with_config).args, ["-filename", "/a.sh"]);
    }

    #[test]
    fn auto_plans_use_the_usual_tool_and_prettier_only_when_asked_for() {
        let reader = files(&[]);
        let plan_for = |reader: &Files, path: &str| plan(reader, Path::new(path), FormatterChoice::Auto, &opts());
        assert!(matches!(plan_for(&reader, "/p/a.rs"), FormatPlan::Programs { explicit: false, ref commands } if commands[0].tool == RUSTFMT));
        match plan_for(&reader, "/p/a.py") {
            FormatPlan::Programs { commands, .. } => assert_eq!(commands.iter().map(|c| c.tool).collect::<Vec<_>>(), [RUFF, BLACK]),
            other => panic!("{other:?}"),
        }
        // Nothing asks for Prettier: the server formats TypeScript.
        assert_eq!(plan_for(&reader, "/p/a.ts"), FormatPlan::LanguageServer);
        for configured in [
            files(&[("/p/.prettierrc", "{}")]),
            files(&[("/p/package.json", "{\"devDependencies\": {\"prettier\": \"3\"}}")]),
        ] {
            assert!(matches!(plan_for(&configured, "/p/src/a.ts"), FormatPlan::Programs { ref commands, .. } if commands[0].tool == PRETTIER));
        }
        assert_eq!(plan_for(&reader, "/p/notes.txt"), FormatPlan::LanguageServer);
    }

    #[test]
    fn a_chosen_tool_is_used_even_without_a_project_signal() {
        let chosen = plan(&files(&[]), Path::new("/p/a.ts"), FormatterChoice::Tool(PRETTIER), &opts());
        assert!(matches!(chosen, FormatPlan::Programs { explicit: true, .. }));
        assert_eq!(plan(&files(&[]), Path::new("/p/a.rs"), FormatterChoice::LanguageServer, &opts()), FormatPlan::LanguageServer);
    }

    struct Scripted(Mutex<Vec<Result<String, FormatFailure>>>);

    impl FormatRunner for Scripted {
        fn run(&self, _: &FormatCommand, _: &str) -> Result<String, FormatFailure> {
            self.0.lock().unwrap().remove(0)
        }
    }

    fn two_commands() -> Vec<FormatCommand> {
        let reader = files(&[]);
        vec![build_command(RUFF, Path::new("/a.py"), &reader, &opts()), build_command(BLACK, Path::new("/a.py"), &reader, &opts())]
    }

    #[test]
    fn the_next_tool_runs_when_one_is_missing() {
        let runner = Scripted(Mutex::new(vec![Err(FormatFailure::NotInstalled), Ok("x = 1\n".into())]));
        assert_eq!(run_programs(&runner, &two_commands(), "x=1", false), Ok(Some((BLACK, "x = 1\n".to_owned()))));
    }

    #[test]
    fn a_failing_tool_is_reported_and_ends_the_search() {
        let runner = Scripted(Mutex::new(vec![Err(FormatFailure::Failed("cannot parse".into()))]));
        assert_eq!(run_programs(&runner, &two_commands(), "x=", false), Err("ruff: cannot parse".to_owned()));
    }

    #[test]
    fn nothing_installed_is_an_error_only_when_the_tool_was_chosen() {
        let missing = || Scripted(Mutex::new(vec![Err(FormatFailure::NotInstalled), Err(FormatFailure::NotInstalled)]));
        assert_eq!(run_programs(&missing(), &two_commands(), "x", false), Ok(None));
        assert_eq!(run_programs(&missing(), &two_commands()[..1], "x", true), Err("ruff is not installed".to_owned()));
    }

    #[test]
    fn empty_output_for_real_input_is_refused() {
        let runner = Scripted(Mutex::new(vec![Ok(String::new())]));
        assert!(run_programs(&runner, &two_commands()[..1], "x = 1\n", false).is_err());
        let runner = Scripted(Mutex::new(vec![Ok(String::new())]));
        assert_eq!(run_programs(&runner, &two_commands()[..1], "  \n", false), Ok(Some((RUFF, String::new()))));
    }
}
