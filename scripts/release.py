#!/usr/bin/env python3
"""Work out the next version from the Conventional Commits since the last release, write it to
Cargo.toml, Cargo.lock and CHANGELOG.md, commit, tag and push.

  fix: / perf:               patch   (0.4.0 -> 0.4.1)
  feat:                      minor   (0.4.0 -> 0.5.0)
  feat!: / BREAKING CHANGE:  major   (a minor one while below 1.0.0)
  anything else              no release

Run by .github/workflows/release.yml on every push to main. Writes `release=` and `tag=` to
$GITHUB_OUTPUT and the notes of the release to release-notes.md.
DRY_RUN=1 changes the files but does not commit, tag or push.
"""
import datetime
import os
import re
import subprocess
import sys

REPO = os.environ.get("GITHUB_REPOSITORY", "Chief-spartan-117/Rustcode")
HEADER = re.compile(r"^(\w+)(?:\([^)]*\))?(!)?: (.+)$")
SECTIONS = [("feat", "Features"), ("fix", "Bug Fixes"), ("perf", "Performance Improvements")]


def git(*args):
    return subprocess.check_output(["git", *args], text=True).rstrip("\n")


def commits_since_last_release():
    try:
        last = git("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*")
    except subprocess.CalledProcessError:
        last = None
    log = git("log", "--format=%H%x1f%s%x1f%b%x1e", f"{last}..HEAD" if last else "HEAD")
    found = []
    for record in log.split("\x1e"):
        # Not str.strip(): it counts the \x1f separator as white space and would eat an empty body.
        record = record.strip("\n")
        if not record:
            continue
        sha, subject, body = record.split("\x1f")
        header = HEADER.match(subject)
        if header and not subject.startswith("chore(release)"):
            kind, bang, text = header.groups()
            found.append((sha, kind, bool(bang) or "BREAKING CHANGE" in body, text))
    return last, found


def next_version(current, commits):
    major, minor, patch = current
    if any(breaking for _, _, breaking, _ in commits):
        return (major, minor + 1, 0) if major == 0 else (major + 1, 0, 0)
    if any(kind == "feat" for _, kind, _, _ in commits):
        return major, minor + 1, 0
    if any(kind in ("fix", "perf") for _, kind, _, _ in commits):
        return major, minor, patch + 1
    return None


def notes(last, tag, commits):
    today = datetime.date.today().isoformat()
    link = f"[{tag[1:]}](https://github.com/{REPO}/compare/{last}...{tag})" if last else tag[1:]
    out = [f"## {link} ({today})", ""]
    for kind, title in SECTIONS:
        lines = [f"* {text} ([{sha[:7]}](https://github.com/{REPO}/commit/{sha}))" for sha, k, _, text in commits if k == kind]
        if lines:
            out += ["", f"### {title}", ""] + lines
    return "\n".join(out) + "\n"


def write(path, change):
    with open(path) as file:
        text = file.read()
    with open(path, "w") as file:
        file.write(change(text))


def main():
    cargo = open("Cargo.toml").read()
    found = re.search(r'^version = "(\d+)\.(\d+)\.(\d+)"', cargo, re.M)
    current = tuple(int(n) for n in found.groups())
    last, commits = commits_since_last_release()
    new = next_version(current, commits)
    out = os.environ.get("GITHUB_OUTPUT")

    def output(text):
        if out:
            with open(out, "a") as file:
                file.write(text)

    if new is None:
        print("no fix, feat or perf commit since the last release: nothing to release")
        output("release=false\n")
        return
    old_text, new_text = ".".join(map(str, current)), ".".join(map(str, new))
    tag = f"v{new_text}"
    entry = notes(last, tag, commits)

    write("Cargo.toml", lambda t: t.replace(f'version = "{old_text}"', f'version = "{new_text}"', 1))
    # Only the workspace's own packages change in Cargo.lock; no dependency is touched.
    subprocess.run(["cargo", "update", "--workspace"], check=True)
    write("CHANGELOG.md", lambda t: t.replace("\n## [", "\n" + entry + "\n## [", 1) if "\n## [" in t else t + "\n" + entry)
    with open("release-notes.md", "w") as file:
        file.write(entry.split("\n", 2)[2])
    print(f"{old_text} -> {new_text}")

    if not os.environ.get("DRY_RUN"):
        git("config", "user.name", "github-actions[bot]")
        git("config", "user.email", "41898282+github-actions[bot]@users.noreply.github.com")
        git("add", "Cargo.toml", "Cargo.lock", "CHANGELOG.md")
        git("commit", "-m", f"chore(release): {tag} [skip ci]")
        git("tag", tag)
        git("push", "--atomic", "origin", "HEAD:main", tag)
    output(f"release=true\ntag={tag}\n")


if __name__ == "__main__":
    sys.exit(main())
