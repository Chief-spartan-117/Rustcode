# Releases

A release is made by itself on every push to `main`, with no pull request. `scripts/release.py` reads
the [Conventional Commits](https://www.conventionalcommits.org) since the last tag:

| Commits since the last release | New version |
| --- | --- |
| `fix:` or `perf:` | patch (0.4.0 to 0.4.1) |
| `feat:` | minor (0.4.0 to 0.5.0) |
| `feat!:` or a `BREAKING CHANGE:` footer | major (a minor one while below 1.0.0) |
| only `docs:`, `chore:`, `ci:`, `test:`, `refactor:` ... | no release |

It writes the version to `Cargo.toml` and `Cargo.lock` and the entry to `CHANGELOG.md`, commits
`chore(release): vX.Y.Z [skip ci]` to `main`, tags it and creates the GitHub release. The macOS and
Windows builds then attach their files to it.

## One setting to change on GitHub

The release commit goes straight to `main`, which a rule that asks for a pull request or a passing
check refuses. Under Settings, Rules, Rulesets, open the rule for `main` and add **GitHub Actions**
(or Repository admin) to its **Bypass list**. Without it the `version` job fails when it pushes.
Your own pushes to `main` need the same bypass; otherwise keep using pull requests for them.

# Signing releases

The release workflow signs and notarizes when the secrets below exist. Without them it builds unsigned files.

## macOS (Apple Developer Program, 99 USD per year)

1. In your Apple Developer account, create a **Developer ID Application** certificate.
2. Export it from Keychain Access as a `.p12` file with a password.
3. Create an app-specific password at appleid.apple.com.
4. Add these repository secrets (Settings, Secrets and variables, Actions):

| Secret | Value |
| --- | --- |
| `MACOS_CERTIFICATE` | `base64 -i cert.p12 \| pbcopy` |
| `MACOS_CERTIFICATE_PASSWORD` | the `.p12` password |
| `MACOS_SIGNING_IDENTITY` | `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | your Apple ID email |
| `APPLE_TEAM_ID` | the 10-character team ID |
| `APPLE_APP_PASSWORD` | the app-specific password |

The script signs the app with the hardened runtime, notarizes and staples the app and the `.dmg`, and checks both with `spctl`.

Local build: `CODESIGN_IDENTITY="Developer ID Application: ..." NOTARY_PROFILE=<profile> scripts/bundle-macos.sh release --dist`

## Windows (Authenticode)

Buy a code signing certificate and export it as `.pfx`.

| Secret | Value |
| --- | --- |
| `WINDOWS_CERTIFICATE` | base64 of the `.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | the `.pfx` password |

`signtool` signs `rustcode.exe` and the installer with a SHA-256 digest and a timestamp.

## Checksums

Every release file has a `.sha256` file. Check a download with `shasum -a 256 -c <file>.sha256`.
