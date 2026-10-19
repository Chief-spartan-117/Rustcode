# Extension survey

Downloads the most downloaded extensions of Open VSX and starts each one in the extension host,
to see which fail to activate and why.

```sh
mkdir /tmp/survey && cd /tmp/survey
python3 /path/to/rustcode/scripts/extension-survey/fetch.py 80   # downloads into ./ext, writes list.json
python3 /path/to/rustcode/scripts/extension-survey/run.py        # prints ok/ERR per extension, writes results.json
python3 /path/to/rustcode/scripts/extension-survey/run.py redhat.vscode-yaml   # only some
```

Each extension runs alone, for seven seconds, in a process of its own (`A+B` runs two together).
What fails because of the machine (no Java, no R, a native module) shows up too: read the errors.
The unpacked extensions can also be used by `crates/infrastructure/tests/real_extensions.rs`
(set `RUSTCODE_REAL_EXTENSIONS` to the `ext` folder).
