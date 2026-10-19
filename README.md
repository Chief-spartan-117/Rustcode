# RustCode

A code editor written in Rust, without Electron. The UI is built on [GPUI](https://crates.io/crates/gpui). The goal is VS Code compatibility: VS Code themes, key bindings and extensions, with extensions running in a Node.js extension host.

## Features

- Text editing with multiple selections, find and replace, folding, word wrap and a minimap
- Syntax highlighting (TextMate grammars) and VS Code color themes
- Language servers: completion, hover with problems, go to definition, references, rename, code actions, formatting
- Jupyter notebooks (`.ipynb`): run cells, kernel picker (Python environments and Jupyter kernels), cell toolbar and menu
- Integrated terminal, debugger (DAP), Git view
- Flutter and Dart: run, debug and device picker
- Workspaces with several folders, a project switcher, and a choice to open projects in a new or the same window

## Layout

```
crates/domain          text buffer, editor, selections, history, themes
crates/application     use cases and ports (traits)
crates/infrastructure  adapters: file system, language servers, kernels, Git, DAP
crates/presentation    GPUI views
apps/rustcode          composition root; starts the UI
docs/ROADMAP.md        plan and status
```

Dependencies point inward: `presentation` and `infrastructure` depend on `application`, which depends on `domain`.

## Build and run

Needs a recent stable Rust toolchain.

```sh
cargo run -p rustcode            # start the editor
cargo run -p rustcode -- <path>  # open a file or folder
cargo test --workspace           # run the tests
```

On macOS, `scripts/bundle-macos.sh` builds `RustCode.app` (see the comments at the top of that script for options).

Notebooks run Python with the built-in driver. Other kernels need `jupyter_client` and the kernel installed (for example `pip install jupyter_client ipykernel`).

## Status

Work in progress. See [docs/ROADMAP.md](docs/ROADMAP.md).

## License

MIT
