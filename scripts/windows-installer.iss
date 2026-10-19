; Inno Setup script: builds RustCode-<version>-windows-x64-setup.exe.
; Run from the repository root after `cargo build --release -p rustcode`:
;   iscc /DAppVersion=0.2.0 scripts/windows-installer.iss
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{8C2B6D1E-5A8F-4E27-9B0A-52C0D7A1F3E4}
AppName=RustCode
AppVersion={#AppVersion}
AppPublisher=Chief-spartan-117
DefaultDirName={autopf}\RustCode
DefaultGroupName=RustCode
UninstallDisplayIcon={app}\rustcode.exe
OutputDir=..\target\dist
OutputBaseFilename=RustCode-{#AppVersion}-windows-x64-setup
SetupIconFile=..\apps\rustcode\assets\RustCode.ico
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog

[Files]
Source: "..\target\release\rustcode.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\RustCode"; Filename: "{app}\rustcode.exe"
Name: "{autodesktop}\RustCode"; Filename: "{app}\rustcode.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Run]
Filename: "{app}\rustcode.exe"; Description: "Start RustCode"; Flags: nowait postinstall skipifsilent
