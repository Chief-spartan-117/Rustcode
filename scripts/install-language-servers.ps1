# Installs the language servers RustCode uses, from their official releases and package
# registries, for this computer. Safe to run again: it brings them up to date.
#   .\scripts\install-language-servers.ps1                 all of them
#   .\scripts\install-language-servers.ps1 markdown lua    only these
# Set $env:RUSTCODE_BIN to the program if it is not on the PATH. Needs curl.exe and tar.exe
# (both come with Windows 10 and later), and npm, rustup or go for the servers that use them.
param([string[]]$Servers = @())
$ErrorActionPreference = 'Stop'
$bin = $env:RUSTCODE_BIN
if (-not $bin) {
    $found = Get-Command rustcode -ErrorAction SilentlyContinue
    if ($found) { $bin = $found.Source }
    elseif (Test-Path "$env:LOCALAPPDATA\Programs\RustCode\rustcode.exe") { $bin = "$env:LOCALAPPDATA\Programs\RustCode\rustcode.exe" }
    elseif (Test-Path 'target\release\rustcode.exe') { $bin = 'target\release\rustcode.exe' }
}
if (-not $bin) { Write-Error 'cannot find rustcode: put it on the PATH or set $env:RUSTCODE_BIN'; exit 1 }
& $bin --install-language-servers @Servers
exit $LASTEXITCODE
