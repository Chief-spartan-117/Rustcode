# Security

## Reporting a problem

Please report a security problem privately, with GitHub's "Report a vulnerability" button on the Security tab of this repository. Do not open a public issue for it.

## What the app protects

- **Folders you open can not run code by themselves.** `cargo check`, language servers, extensions, tools in `node_modules/.bin`, Python virtual environments in the folder and the hooks of a git repository stay off until you run "Workspace: Trust This Folder". The trusted folders are listed in `~/.rustcode/trusted-folders`.
- **Downloads are checked.** Only https is fetched. Updates need a SHA-256 checksum and a valid code signature on the unpacked app. Language servers and the JavaScript debugger are checked against the digest GitHub lists for the release file.
- **Archives** are unpacked without paths that leave the folder, without setuid bits and with a size limit.
- **Edits to files outside the open folders** asked for by a language server or an extension are refused.
- **Notebook HTML output** opens in a page without scripts.

## What it does not protect against

- Extensions run as Node programs with your rights, as in VS Code. They start only in trusted folders, but install only extensions you trust.
- `RUSTCODE_TRUST_ALL=1` turns the folder trust check off.
