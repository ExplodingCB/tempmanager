; Inno Setup script for tempmanager.
;
; Build from the repo root after `cargo build --release`:
;   iscc /DAppVersion=0.1.0 installer\tempmanager.iss
;
; Installs per-user by default (no UAC prompt) into %LOCALAPPDATA%\Programs.
; Pass /ALLUSERS to install for every user under Program Files instead.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\x86_64-pc-windows-gnu\release\tempmanager.exe"
#endif

#define AppName "tempmanager"
#define AppPublisher "ExplodingCB"
#define AppURL "https://github.com/ExplodingCB/tempmanager"
#define AppExe "tempmanager.exe"

[Setup]
AppId={{9A271373-8B4F-44D1-8FEB-4FB10E2550DC}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\LICENSE
SetupIconFile=..\assets\tempmanager.ico
UninstallDisplayIcon={app}\tempmanager.ico
UninstallDisplayName={#AppName}
OutputDir=..\dist
OutputBaseFilename=tempmanager-setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion
Source: "..\assets\tempmanager.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\SETUP.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\tempmanager.ico"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\tempmanager.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
// The app lives in the tray with no visible window, so stop any copy that is
// running from the install folder before files are replaced or removed. Only
// processes launched from {app} are touched; a copy elsewhere keeps running.
procedure StopInstalledCopy();
var
  ResultCode: Integer;
  Cmd: String;
begin
  Cmd := '-NoProfile -NonInteractive -Command "Get-Process tempmanager -ErrorAction SilentlyContinue | ' +
         'Where-Object { $_.Path -eq ''' + ExpandConstant('{app}\{#AppExe}') + ''' } | ' +
         'Stop-Process -Force -ErrorAction SilentlyContinue"';
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'), Cmd, '', SW_HIDE,
       ewWaitUntilTerminated, ResultCode);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopInstalledCopy();
  Result := '';
end;

// "Start with Windows" writes an HKCU Run value or a logon scheduled task named
// tempmanager. Remove them on uninstall, but only when they point at this
// install, so a copy of the exe run from somewhere else keeps its autostart.
procedure RemoveAutostart();
var
  Value, Cmd: String;
  ResultCode: Integer;
begin
  if RegQueryStringValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'tempmanager', Value) then
    if Pos(Lowercase(ExpandConstant('{app}\{#AppExe}')), Lowercase(Value)) > 0 then
      RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'tempmanager');

  // Deleting an elevated task needs admin; a per-user uninstall does its best.
  Cmd := '-NoProfile -NonInteractive -Command "$t = Get-ScheduledTask -TaskName tempmanager -ErrorAction SilentlyContinue; ' +
         'if ($t -and ($t.Actions.Execute -join '''') -like ''*' + ExpandConstant('{app}\{#AppExe}') + '*'') ' +
         '{ Unregister-ScheduledTask -TaskName tempmanager -Confirm:$false -ErrorAction SilentlyContinue }"';
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'), Cmd, '', SW_HIDE,
       ewWaitUntilTerminated, ResultCode);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    StopInstalledCopy();
    RemoveAutostart();
  end;
end;
