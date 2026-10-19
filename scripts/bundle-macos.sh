#!/bin/sh
# Builds RustCode.app from the compiled program, with its icon.
#   scripts/bundle-macos.sh            release build, app in target/bundle/RustCode.app
#   scripts/bundle-macos.sh debug      use the debug build (faster to produce)
#   scripts/bundle-macos.sh release --with-servers
#                                      also download the language servers into the app
#                                      (Contents/Resources/servers), for this Mac's CPU, from their
#                                      official releases. Without it they install themselves into
#                                      ~/.rustcode/servers the first time a file needs one.
#   scripts/bundle-macos.sh release --dist
#                                      also make the release files in target/dist: the zip the
#                                      updater downloads (RustCode-<version>-macos-<arch>.zip),
#                                      its .sha256, and a .dmg. Upload all three to a GitHub release.
# Environment: CODESIGN_IDENTITY signs the app and the .dmg ("Developer ID Application: ..."; "-" signs ad hoc).
#              Notarization (needs a real identity) uses one of:
#                NOTARY_PROFILE   a `xcrun notarytool store-credentials` name, or
#                APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD   an app-specific password.
#              It notarizes and staples both the app (zip) and the .dmg.
#              Every file in target/dist gets a .sha256 next to it.
set -eu

# notarize FILE: submit to Apple and wait; a rejection makes the script stop.
notarize() {
    if [ -n "${NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$1" --keychain-profile "$NOTARY_PROFILE" --wait
    else
        xcrun notarytool submit "$1" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
            --password "$APPLE_APP_PASSWORD" --wait
    fi
}

NOTARIZE=no
if [ -n "${NOTARY_PROFILE:-}" ] || { [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; }; then
    NOTARIZE=yes
fi
PROFILE=${1:-release}
WITH_SERVERS=no
DIST=no
for arg in "$@"; do
    [ "$arg" = "--with-servers" ] && WITH_SERVERS=yes
    [ "$arg" = "--dist" ] && DIST=yes
done
case "$PROFILE" in --*) PROFILE=release ;; esac
ASSETS=apps/rustcode/assets
APP=target/bundle/RustCode.app

if [ "$PROFILE" = "release" ]; then
    cargo build --release -p rustcode
else
    cargo build -p rustcode
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$PROFILE/rustcode" "$APP/Contents/MacOS/rustcode"
cp "$ASSETS/RustCode.icns" "$APP/Contents/Resources/RustCode.icns"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>RustCode</string>
    <key>CFBundleDisplayName</key><string>RustCode</string>
    <key>CFBundleIdentifier</key><string>dev.rustcode.editor</string>
    <key>CFBundleExecutable</key><string>rustcode</string>
    <key>CFBundleIconFile</key><string>RustCode</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>${VERSION:-0.1.0}</string>
    <key>CFBundleVersion</key><string>${VERSION:-0.1.0}</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
    <!-- Pages of tools that serve themselves on this machine (Flutter DevTools) load over http. -->
    <key>NSAppTransportSecurity</key><dict><key>NSAllowsLocalNetworking</key><true/></dict>
</dict>
</plist>
PLIST
if [ "$WITH_SERVERS" = "yes" ]; then
    # rust-analyzer is left out: it comes from rustup, which would change this Mac's toolchain.
    # RUSTCODE_BUNDLE_SERVERS chooses others, for example "markdown toml".
    # shellcheck disable=SC2086
    RUSTCODE_SERVERS_DIR="$APP/Contents/Resources/servers" "target/$PROFILE/rustcode" --install-language-servers ${RUSTCODE_BUNDLE_SERVERS:-all -rust-analyzer} || \
        echo "some language servers could not be added; they will install themselves when first needed"
fi
# Ad hoc signing ("-") needs no Apple account; it is the default. A real identity adds the
# hardened runtime and a secure timestamp.
CODESIGN_IDENTITY=${CODESIGN_IDENTITY:--}
if [ "$CODESIGN_IDENTITY" = "-" ]; then
    SIGN_OPTS="--timestamp=none"
    DMG_OPTS="--timestamp=none"
else
    SIGN_OPTS="--options runtime --timestamp"
    DMG_OPTS="--timestamp"
fi
if [ -n "$CODESIGN_IDENTITY" ]; then
    # shellcheck disable=SC2086
    codesign --force --deep $SIGN_OPTS --sign "$CODESIGN_IDENTITY" "$APP"
    codesign --verify --deep --strict "$APP"
fi
echo "built $APP"

if [ "$DIST" = "yes" ]; then
    DIST_DIR=target/dist
    ARCH=$(uname -m)
    NAME="RustCode-${VERSION:-0.1.0}-macos-$ARCH"
    rm -rf "$DIST_DIR"
    mkdir -p "$DIST_DIR"
    # ditto keeps the bundle's metadata; the updater unpacks the zip with `ditto -x -k`.
    ditto -c -k --keepParent "$APP" "$DIST_DIR/$NAME.zip"
    if [ "$NOTARIZE" = "yes" ]; then
        notarize "$DIST_DIR/$NAME.zip"
        xcrun stapler staple "$APP"
        rm "$DIST_DIR/$NAME.zip"
        ditto -c -k --keepParent "$APP" "$DIST_DIR/$NAME.zip"
    fi
    STAGE=$(mktemp -d)
    cp -R "$APP" "$STAGE/"
    ln -s /Applications "$STAGE/Applications"
    hdiutil create -volname RustCode -srcfolder "$STAGE" -ov -format UDZO "$DIST_DIR/$NAME.dmg" >/dev/null
    rm -rf "$STAGE"
    # shellcheck disable=SC2086
    codesign --force $DMG_OPTS --sign "$CODESIGN_IDENTITY" "$DIST_DIR/$NAME.dmg"
    if [ "$NOTARIZE" = "yes" ]; then
        notarize "$DIST_DIR/$NAME.dmg"
        xcrun stapler staple "$DIST_DIR/$NAME.dmg"
        spctl --assess --type open --context context:primary-signature --verbose=2 "$DIST_DIR/$NAME.dmg"
        spctl --assess --type execute --verbose=2 "$APP"
    fi
    # Checksums come last: stapling changes the files.
    (cd "$DIST_DIR" && shasum -a 256 "$NAME.zip" > "$NAME.zip.sha256" && shasum -a 256 "$NAME.dmg" > "$NAME.dmg.sha256")
    echo "release files in $DIST_DIR:"
    ls -1 "$DIST_DIR"
fi
