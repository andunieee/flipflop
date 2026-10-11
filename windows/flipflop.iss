; Inno Setup script for the Windows installer. Built in CI (release.yml):
;   iscc /DAppVersion=1.2.3 /DSourceExe=path\to\flipflop.exe /DOutputDir=dist windows\flipflop.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\flipflop.exe"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

[Setup]
; Never change AppId: Windows uses it to recognise upgrades of the same app.
AppId={{833EBC89-8908-4FE8-95C3-48D01CAEE821}
AppName=flipflop
AppVersion={#AppVersion}
AppVerName=flipflop {#AppVersion}
AppPublisher=flipflop
AppPublisherURL=https://github.com/andunieee/flipflop
AppSupportURL=https://github.com/andunieee/flipflop/issues
DefaultDirName={autopf}\flipflop
DefaultGroupName=flipflop
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
; Per-user install by default (no admin prompt); the user can still pick
; "install for all users" in the dialog.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=flipflop.ico
UninstallDisplayIcon={app}\flipflop.exe
OutputDir={#OutputDir}
OutputBaseFilename=flipflop-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\flipflop"; Filename: "{app}\flipflop.exe"
Name: "{autodesktop}\flipflop"; Filename: "{app}\flipflop.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\flipflop.exe"; Description: "{cm:LaunchProgram,flipflop}"; Flags: nowait postinstall skipifsilent

; Pairings, identity and history live in %APPDATA%\flipflop and are kept on
; uninstall, so reinstalling doesn't lose them.
