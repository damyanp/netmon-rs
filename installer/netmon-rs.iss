; Inno Setup script for Network Monitor (netmon-rs).
;
; Build with the helper script (recommended):
;     powershell -ExecutionPolicy Bypass -File installer\build-installer.ps1
; or directly, after `cargo build --release`:
;     iscc /DAppVersion=0.1.0 installer\netmon-rs.iss
;
; Produces a per-user installer (no admin required) under installer\Output\.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

#define AppName "Network Monitor"
#define AppExeName "netmon-rs.exe"
#define AppPublisher "Damyan Pepper"
#define AppURL "https://github.com/damyanp/netmon-rs"

[Setup]
; Keep this GUID stable across versions so upgrades replace, not duplicate.
AppId={{BD68135A-D201-49F9-9775-BC0FCE24AA11}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}
; Per-user install: no elevation, and the app can write settings.json /
; history.json next to its exe (it stores data beside the executable).
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\{#AppExeName}
SetupIconFile=..\assets\netmon.ico
OutputDir=Output
OutputBaseFilename=NetworkMonitor-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\microsoft.windowsappruntime.bootstrap.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\resources.pri"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Remove the app's own data files (created next to the exe at runtime).
Type: files; Name: "{app}\settings.json"
Type: files; Name: "{app}\history.json"
