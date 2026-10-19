//! `cargo clippy` as the project's check command, run for real (skipped without clippy).

use rustcode_application::DiagnosticsProvider;
use rustcode_infrastructure::CargoCheck;

#[test]
fn clippy_lints_come_back_named_clippy_and_plain_check_leaves_them_out() {
    let clippy_installed = std::process::Command::new("cargo")
        .args(["clippy", "--version"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !clippy_installed {
        eprintln!("cargo clippy is not installed; skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("rustcode-clippy-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> i32 {\n    return 1;\n}\n").unwrap();

    let linted = CargoCheck.check_with(&dir, "clippy");
    let checked = CargoCheck.check_with(&dir, "check");
    let _ = std::fs::remove_dir_all(&dir);

    let linted = linted.unwrap();
    let needless = linted.iter().find(|d| d.message.contains("clippy::needless_return")).unwrap_or_else(|| panic!("{linted:?}"));
    assert_eq!((needless.source.as_str(), needless.line), ("clippy", 1));
    assert!(checked.unwrap().is_empty(), "plain `cargo check` has nothing to say about this code");
}
