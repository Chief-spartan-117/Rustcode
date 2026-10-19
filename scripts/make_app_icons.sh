#!/bin/sh
# Renders apps/rustcode/assets/app-icon.svg into every icon file the platforms want:
#   app-icon-512.png  embedded in the program, used for the dock icon while it runs
#   app-icon-1024.png the master image
#   RustCode.icns     macOS application bundles (needs `iconutil`, so only on macOS)
#   RustCode.ico      Windows executables and shortcuts (needs Python with Pillow)
# Run from the repository root after changing the SVG.
set -eu

ASSETS=apps/rustcode/assets
RENDER=scripts/svg-render/target/release/svg-render

if [ ! -x "$RENDER" ]; then
    cargo build --release --manifest-path scripts/svg-render/Cargo.toml
fi

$RENDER "$ASSETS/app-icon.svg" "$ASSETS/app-icon-1024.png" 1024
$RENDER "$ASSETS/app-icon.svg" "$ASSETS/app-icon-512.png" 512

if command -v iconutil >/dev/null 2>&1; then
    SET=$(mktemp -d)/RustCode.iconset
    mkdir -p "$SET"
    for size in 16 32 128 256 512; do
        $RENDER "$ASSETS/app-icon.svg" "$SET/icon_${size}x${size}.png" "$size"
        $RENDER "$ASSETS/app-icon.svg" "$SET/icon_${size}x${size}@2x.png" "$((size * 2))"
    done
    iconutil -c icns "$SET" -o "$ASSETS/RustCode.icns"
fi

python3 - <<'PY'
from PIL import Image
image = Image.open("apps/rustcode/assets/app-icon-1024.png")
sizes = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
image.save("apps/rustcode/assets/RustCode.ico", sizes=sizes)
PY
echo "icons written to $ASSETS"
