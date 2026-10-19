#!/usr/bin/env python3
"""Imports the Material Icon Theme (https://github.com/material-extensions/vscode-material-icon-theme,
MIT) as RustCode's file and folder icons.

    npm pack material-icon-theme && tar xzf material-icon-theme-*.tgz
    python3 scripts/material_icons.py package
    python3 scripts/generate_icons.py      # lists the icons in apps/rustcode/src/assets.rs

Copies the SVGs to apps/rustcode/assets/icons and writes apps/rustcode/assets/icon-theme.json, the
theme's own file name, extension and folder tables (light and high contrast variants and language
ids are not used by the program).
"""
import json
import os
import shutil
import sys

package = sys.argv[1] if len(sys.argv) > 1 else "package"
assets = "apps/rustcode/assets"
icons = f"{assets}/icons"

manifest = json.load(open(f"{package}/dist/material-icons.json"))
keep = ["iconDefinitions", "file", "folder", "folderExpanded", "fileExtensions", "fileNames", "folderNames", "folderNamesExpanded"]
theme = {key: manifest[key] for key in keep}
# `./../icons/x.svg` in the package becomes `./icons/x.svg` beside the theme file.
for definition in theme["iconDefinitions"].values():
    definition["iconPath"] = "./icons/" + definition["iconPath"].rsplit("/", 1)[-1]

shutil.rmtree(icons, ignore_errors=True)
os.makedirs(icons)
used = {d["iconPath"].rsplit("/", 1)[-1] for d in theme["iconDefinitions"].values()}
for name in sorted(used):
    shutil.copy(f"{package}/icons/{name}", f"{icons}/{name}")
shutil.copy(f"{package}/LICENSE", f"{icons}/MATERIAL-ICON-THEME-LICENSE.txt")
json.dump(theme, open(f"{assets}/icon-theme.json", "w"), separators=(",", ":"), sort_keys=True)
version = json.load(open(f"{package}/package.json"))["version"]
open(f"{icons}/README.md", "w").write(
    f"Material Icon Theme {version} by Philipp Kief and contributors, MIT license (see the license file here).\n"
    "Imported by scripts/material_icons.py.\n"
)
print(len(used), "icons,", len(theme["fileExtensions"]), "extensions,", len(theme["fileNames"]), "file names,", len(theme["folderNames"]), "folder names")
