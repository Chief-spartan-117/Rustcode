#!/bin/sh
# Installs the language servers RustCode uses, from their official releases and package
# registries, for this computer's OS and CPU. Safe to run again: it brings them up to date.
#   scripts/install-language-servers.sh                 all of them
#   scripts/install-language-servers.sh markdown lua    only these
# Set RUSTCODE_BIN to the program if it is not on the PATH; RUSTCODE_SERVERS_DIR to install
# somewhere other than ~/.rustcode/servers. Needs curl and tar (and npm, rustup or go for the
# servers that come from them). RustCode installs a server by itself the first time a file needs
# it, so this is for installers, offline preparation and CI.
set -eu

BIN=${RUSTCODE_BIN:-}
if [ -z "$BIN" ]; then
    for candidate in \
        "$(command -v rustcode 2>/dev/null || true)" \
        "/Applications/RustCode.app/Contents/MacOS/rustcode" \
        "$HOME/Applications/RustCode.app/Contents/MacOS/rustcode" \
        "target/release/rustcode" "target/debug/rustcode"; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then BIN=$candidate; break; fi
    done
fi
if [ -z "$BIN" ]; then
    echo "cannot find rustcode: put it on the PATH or set RUSTCODE_BIN" >&2
    exit 1
fi
exec "$BIN" --install-language-servers "$@"
