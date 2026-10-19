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
# Environment: CODESIGN_IDENTITY signs the app ("Developer ID Application: ..."; "-" signs ad hoc).
#              NOTARY_PROFILE (a `xcrun notarytool store-credentials` name) notarizes the zip too.
set -eu

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
if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    codesign --force --deep --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP"
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
    if [ -n "${NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$DIST_DIR/$NAME.zip" --keychain-profile "$NOTARY_PROFILE" --wait
        xcrun stapler staple "$APP"
        rm "$DIST_DIR/$NAME.zip"
        ditto -c -k --keepParent "$APP" "$DIST_DIR/$NAME.zip"
    fi
    (cd "$DIST_DIR" && shasum -a 256 "$NAME.zip" > "$NAME.zip.sha256")
    STAGE=$(mktemp -d)
    cp -R "$APP" "$STAGE/"
    ln -s /Applications "$STAGE/Applications"
    hdiutil create -volname RustCode -srcfolder "$STAGE" -ov -format UDZO "$DIST_DIR/$NAME.dmg" >/dev/null
    rm -rf "$STAGE"
    echo "release files in $DIST_DIR:"
    ls -1 "$DIST_DIR"
fi
