# Contributing to RustCode

Thanks for helping. This page says how to build the project, how to write commits, and how a change becomes a release.

## Build and test

```sh
cargo run -p rustcode            # start the editor
cargo test --workspace           # run the tests
```

The code is split in layers; dependencies point inward: `presentation` and `infrastructure` depend on `application`, which depends on `domain`. See `docs/ROADMAP.md`. Keep new code in the layer it belongs to: no I/O or UI types in `domain`, no GPUI in `application`.

Some UI tests that click the in-window menu bar fail on macOS today (the menu starts after the traffic lights). That is known; do not add new failures.

## Branches and pull requests

1. Branch from `main`: `git switch -c fix/short-description`.
2. Make the change, with tests where it can be tested.
3. Open a pull request against `main`. Its commits are checked against the rules below.
4. Use "Squash and merge" or "Rebase and merge" so every commit on `main` is a good commit message.

## Commit messages: Conventional Commits

Releases are made from commit messages, so they must look like this:

```
<type>(<optional scope>): <short summary in the imperative>

<optional body: what and why>

<optional footer: BREAKING CHANGE: ..., Closes #123>
```

| Type | Use for | Release |
|------|---------|---------|
| `fix` | a bug fix | patch (0.1.0 → 0.1.1) |
| `feat` | a new feature | minor (0.1.0 → 0.2.0) |
| `feat!`, `fix!`, or a `BREAKING CHANGE:` footer | a change that breaks users | major (a minor one while below 1.0.0) |
| `perf` | a speed-up | patch |
| `docs`, `style`, `refactor`, `test`, `build`, `ci`, `chore` | everything else | none (not listed in the changelog) |

Examples:

```
fix(notebook): keep the kernel when a notebook is reloaded
feat(extensions): install extensions from Open VSX
feat!: drop support for format 3 notebooks
docs: explain the layers in CONTRIBUTING.md
```

## Releases and versions

You never edit the version or `CHANGELOG.md` by hand.

1. When commits reach `main`, the **Release** workflow opens (or updates) a pull request named "chore(main): release x.y.z". It holds the new version in `Cargo.toml` and the new `CHANGELOG.md` entry, worked out from the commit types above.
2. A maintainer merges that pull request when a release is wanted. This creates the tag `vx.y.z` and the GitHub release.
3. The workflow then builds the macOS app (Apple Silicon and Intel) and attaches `RustCode-<version>-macos-<arch>.zip`, its `.sha256` and a `.dmg` to the release. A Windows job attaches `RustCode-<version>-windows-x64-setup.exe` (an installer made with Inno Setup from `scripts/windows-installer.iss`) and a zip holding `rustcode.exe`.
4. Installed copies of RustCode check this repository's releases and offer the new version through the built-in updater. Builds made by the workflow know the repository (`RUSTCODE_UPDATE_REPO`); a build of your own can set it with the `rustcode.update.repository` setting.

### One-time repository setup

For the workflows to work, in the GitHub repository settings:

- *Settings → Actions → General → Workflow permissions*: "Read and write permissions", and tick "Allow GitHub Actions to create and approve pull requests".
- Optional: make the **CI** and **Commit messages** checks required for `main`.
- Optional, to sign and notarize the app: add the secrets used by `scripts/bundle-macos.sh` (`CODESIGN_IDENTITY`, `NOTARY_PROFILE`) and pass them in `.github/workflows/release.yml`.

## Reporting problems

Open an issue with what you did, what you expected, and what happened. Screenshots help for anything visual.
