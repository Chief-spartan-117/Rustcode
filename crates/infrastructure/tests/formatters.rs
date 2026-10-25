//! The real formatter programs, run through the real runner with the command lines this editor
//! builds. A program that is not installed is skipped (the test says so); put them on `PATH`.

use std::path::PathBuf;

use rustcode_application::{
    build_command, run_programs, EditorConfig, FormatOptions, FormatRunner, BLACK, CLANG_FORMAT, ELM_FORMAT, GOFMT, PRETTIER, RUBOCOP, RUFF,
    RUSTFMT, SHFMT, STYLUA, SWIFTFORMAT, TAPLO, TERRAFORM, ZIG, CSHARPIER, DART, GLEAM, GOOGLE_JAVA_FORMAT, KTFMT, MIX, NIXFMT, OCAMLFORMAT,
    ORMOLU, SCALAFMT,
};
use rustcode_infrastructure::{ProgramFormatRunner, StdFileSystem};

struct Project(PathBuf);

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        std::env::set_var("RUSTCODE_TRUST_ALL", "1");
        let dir = std::env::temp_dir().join(format!("rustcode-fmt-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            if let Some(parent) = std::path::Path::new(file).parent() {
                std::fs::create_dir_all(dir.join(parent)).unwrap();
            }
            std::fs::write(dir.join(file), text).unwrap();
        }
        Self(dir)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn options(tab_size: usize) -> FormatOptions {
    FormatOptions { tab_size, insert_spaces: true, editorconfig: EditorConfig::default() }
}

/// Format `input` as the file `name` of a folder that holds `files`; `None` when the tool is not
/// installed.
fn format(tool: &'static str, name: &str, input: &str, files: &[(&str, &str)], options: &FormatOptions) -> Option<String> {
    let project = Project::new(tool, files);
    let path = project.0.join(name);
    let command = build_command(tool, &path, &StdFileSystem, options);
    if !ProgramFormatRunner::new().available(&command) {
        eprintln!("{tool} is not installed; skipped");
        return None;
    }
    let done = run_programs(&ProgramFormatRunner::new(), &[command], input, true).unwrap_or_else(|e| panic!("{e}"));
    Some(done.expect("installed").1)
}

#[test]
fn rustfmt_and_gofmt() {
    if let Some(out) = format(RUSTFMT, "a.rs", "fn  main( ){let x=1;}\n", &[], &options(4)) {
        assert_eq!(out, "fn main() {\n    let x = 1;\n}\n");
    }
    if let Some(out) = format(GOFMT, "a.go", "package main\nfunc  main( ){\nx:=1\n_=x}\n", &[], &options(4)) {
        assert_eq!(out, "package main\n\nfunc main() {\n\tx := 1\n\t_ = x\n}\n");
    }
}

#[test]
fn black_and_ruff_format_python() {
    let input = "x=1\nif x :\n  pass\n";
    let expected = "x = 1\nif x:\n    pass\n";
    if let Some(out) = format(BLACK, "a.py", input, &[], &options(4)) {
        assert_eq!(out, expected);
    }
    if let Some(out) = format(RUFF, "a.py", input, &[], &options(4)) {
        assert_eq!(out, expected);
    }
}

#[test]
fn prettier_uses_the_projects_config() {
    let input = "const  a={b:1,c:'x'}\n";
    // With no config: its defaults.
    if let Some(out) = format(PRETTIER, "a.ts", input, &[("package.json", "{}")], &options(4)) {
        assert_eq!(out, "const a = { b: 1, c: \"x\" };\n");
    }
    // A config of the project decides: single quotes, no semicolons.
    let files = [("package.json", "{}"), (".prettierrc", "{\"singleQuote\": true, \"semi\": false}")];
    if let Some(out) = format(PRETTIER, "a.ts", input, &files, &options(4)) {
        assert_eq!(out, "const a = { b: 1, c: 'x' }\n");
    }
    // CSS and Markdown go through the same program.
    if let Some(out) = format(PRETTIER, "a.css", "a{color:red}\n", &[], &options(4)) {
        assert_eq!(out, "a {\n  color: red;\n}\n");
    }
}

#[test]
fn clang_format_formats_c() {
    if let Some(out) = format(CLANG_FORMAT, "a.c", "int main(){return 0;}\n", &[], &options(4)) {
        assert_eq!(out, "int main() { return 0; }\n");
    }
}

#[test]
fn shfmt_takes_the_editors_indentation() {
    let input = "if true;then\necho hi\nfi\n";
    if let Some(out) = format(SHFMT, "a.sh", input, &[], &options(4)) {
        assert_eq!(out, "if true; then\n    echo hi\nfi\n");
    }
    if let Some(out) = format(SHFMT, "a.sh", input, &[], &options(2)) {
        assert_eq!(out, "if true; then\n  echo hi\nfi\n");
    }
}

#[test]
fn taplo_and_stylua() {
    if let Some(out) = format(TAPLO, "a.toml", "a   =  1\n[b]\nc=2\n", &[], &options(4)) {
        assert_eq!(out, "a = 1\n[b]\nc = 2\n");
    }
    if let Some(out) = format(STYLUA, "a.lua", "local x=1\nprint( x )\n", &[], &options(4)) {
        assert_eq!(out, "local x = 1\nprint(x)\n");
    }
}

#[test]
fn rubocop_formats_with_its_layout_cops_only() {
    // `x=1` is an assignment nothing reads: a correcting run with every cop would delete it.
    let input = "puts  \"a\"\nx=1\ndef f( a )\n  a+1\nend\n";
    if let Some(out) = format(RUBOCOP, "a.rb", input, &[], &options(2)) {
        assert_eq!(out, "puts \"a\"\nx = 1\ndef f(a)\n  a + 1\nend\n");
    }
}

#[test]
fn elm_terraform_swift_and_zig() {
    if let Some(out) = format(ELM_FORMAT, "a.elm", "module Main exposing (..)\nx=1\n", &[], &options(4)) {
        assert_eq!(out, "module Main exposing (..)\n\n\nx =\n    1\n");
    }
    if let Some(out) = format(TERRAFORM, "a.tf", "resource \"a\" \"b\" {\nx   =  1\n}\n", &[], &options(2)) {
        assert_eq!(out, "resource \"a\" \"b\" {\n  x = 1\n}\n");
    }
    if let Some(out) = format(SWIFTFORMAT, "a.swift", "let x=1\nfunc  f( ){ }\n", &[], &options(4)) {
        assert_eq!(out, "let x = 1\nfunc f() {}\n");
    }
    if let Some(out) = format(ZIG, "a.zig", "pub fn main() void{ const x=1; _ = x; }\n", &[], &options(4)) {
        assert_eq!(out, "pub fn main() void {\n    const x = 1;\n    _ = x;\n}\n");
    }
}

#[test]
fn java_and_kotlin() {
    let java = "public class A{public static void main(String[] a){int x=1;}}\n";
    if let Some(out) = format(GOOGLE_JAVA_FORMAT, "A.java", java, &[], &options(4)) {
        assert_eq!(out, "public class A {\n  public static void main(String[] a) {\n    int x = 1;\n  }\n}\n");
    }
    if let Some(out) = format(KTFMT, "A.kt", "fun main(){val x=1\nprintln( x )}\n", &[], &options(4)) {
        assert_eq!(out, "fun main() {\n  val x = 1\n  println(x)\n}\n");
    }
}

#[test]
fn gleam_dart_and_haskell() {
    if let Some(out) = format(GLEAM, "a.gleam", "pub fn main(){let x=1\nx}\n", &[], &options(4)) {
        assert_eq!(out, "pub fn main() {\n  let x = 1\n  x\n}\n");
    }
    if let Some(out) = format(DART, "a.dart", "void main(){var x=1;print( x );}\n", &[], &options(4)) {
        assert_eq!(out, "void main() {\n  var x = 1;\n  print(x);\n}\n");
    }
    if let Some(out) = format(ORMOLU, "a.hs", "module Main where\nmain=putStrLn   \"hi\"\n", &[], &options(4)) {
        assert_eq!(out, "module Main where\n\nmain = putStrLn \"hi\"\n");
    }
}

#[test]
fn csharpier_is_found_under_either_of_its_names() {
    // On the machine these tests were written on only `dotnet-csharpier` exists.
    if let Some(out) = format(CSHARPIER, "A.cs", "class A{void F(){int x=1;}}\n", &[], &options(4)) {
        assert_eq!(out, "class A\n{\n    void F()\n    {\n        int x = 1;\n    }\n}\n");
    }
}

#[test]
fn mix_format_finds_the_project() {
    let files = [
        ("mix.exs", "defmodule M.MixProject do\n  use Mix.Project\n  def project, do: [app: :m, version: \"0.1.0\"]\nend\n"),
        (".formatter.exs", "[inputs: [\"{mix,.formatter}.exs\", \"{config,lib,test}/**/*.{ex,exs}\"]]\n"),
    ];
    if let Some(out) = format(MIX, "lib/m.ex", "defmodule M do\ndef f( x ),do: x+1\nend\n", &files, &options(2)) {
        assert_eq!(out, "defmodule M do\n  def f(x), do: x + 1\nend\n");
    }
}

#[test]
fn scalafmt_reads_the_projects_config() {
    // The config names a version; use the one that is installed.
    let Ok(version) = std::process::Command::new("scalafmt").arg("--version").output() else {
        eprintln!("scalafmt is not installed; skipped");
        return;
    };
    let text = String::from_utf8_lossy(&version.stdout).to_string() + &String::from_utf8_lossy(&version.stderr);
    // The version line is "scalafmt 3.11.5"; the JVM may print warnings around it.
    let Some(version) = text.lines().find_map(|l| l.strip_prefix("scalafmt ")).map(str::trim) else {
        eprintln!("scalafmt is not installed; skipped");
        return;
    };
    let config = format!("version = \"{version}\"\nrunner.dialect = scala3\n");
    if let Some(out) = format(SCALAFMT, "src/A.scala", "object A{def f(x:Int)=x+1}\n", &[(".scalafmt.conf", &config)], &options(2)) {
        assert_eq!(out, "object A { def f(x: Int) = x + 1 }\n");
    }
}

#[test]
fn nixfmt_and_ocamlformat() {
    if let Some(out) = format(NIXFMT, "a.nix", "{a=1;b=  2;}\n", &[], &options(2)) {
        assert_eq!(out, "{\n  a = 1;\n  b = 2;\n}\n");
    }
    let expected = "let x = 1\nlet f y = y + 1\n";
    if let Some(out) = format(OCAMLFORMAT, "a.ml", "let x=1\nlet f  y=y+1\n", &[(".ocamlformat", "profile = default\n")], &options(2)) {
        assert_eq!(out, expected);
    }
    // No `.ocamlformat` above the file: it is still formatted.
    if let Some(out) = format(OCAMLFORMAT, "a.ml", "let x=1\nlet f  y=y+1\n", &[], &options(2)) {
        assert_eq!(out, expected);
    }
}
