; tapbridge installer. Build per architecture with:
;   iscc /DAppArch=arm64 /DAppVersion=<ver> installer\tapbridge.iss
; Produces dist\tapbridge-<ver>-<arch>-setup.exe (see scripts/build-desktop.ps1).
;
; Per-machine install of tapbridge.exe; bundles the pinned, hash-checked
; usbip-win2 installer and runs it silently only when the driver is missing.
; Code signing: set env SIGNTOOL_PFX/SIGNTOOL_PFX_PASSWORD/SIGNTOOL_TIMESTAMP_URL
; before running iscc; when unset the artifacts stay unsigned (open gate).

#define Name "tapbridge"
#ifndef AppArch
  #define AppArch "arm64"
#endif
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
; Exact file name from scripts/fetch-usbip.ps1 (the pinned version's single
; source). [Run] does not expand wildcards, so this must be a real name.
#ifndef UsbipFile
  #error Pass /DUsbipFile=<USBip-...-arch.exe> (scripts/build-desktop.ps1 does this)
#endif

[Setup]
AppId={{7C3D9E44-52D5-4B6E-9A21-4D9E0F5C1A11}}
AppName={#Name}
AppVersion={#AppVersion}
AppPublisher=tapbridge
DefaultDirName={autopf}\{#Name}
DefaultGroupName={#Name}
DisableProgramGroupPage=yes
; per-machine
PrivilegesRequired=admin
OutputDir=..\dist
OutputBaseFilename=tapbridge-{#AppVersion}-{#AppArch}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible arm64
ArchitecturesAllowed=x64compatible arm64
; Signing hook: skipped entirely when the environment is unset.
; Set SIGNTOOL_PFX (path), SIGNTOOL_PFX_PASSWORD and optional
; SIGNTOOL_TIMESTAMP_URL (default http://timestamp.digicert.com).
#if FileExists(GetEnv("SIGNTOOL_PFX"))
SignTool=signtool /p $q{#GetEnv('SIGNTOOL_PFX_PASSWORD')}$q /fd SHA256 /tr $q{#GetEnv('SIGNTOOL_TIMESTAMP_URL')}$q /td SHA256 $f
#endif
UninstallFilesDir={app}\uninstall

[Types]
Name: "full"; Description: "Full installation"; Flags: iscustom

[Components]
Name: "app"; Description: "tapbridge"; Types: full; Flags: fixed
Name: "usbip"; Description: "USB/IP driver (usbip-win2)"; Types: full

[Files]
Source: "..\dist\tapbridge-{#AppVersion}-{#AppArch}\tapbridge.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "licenses\usbip-win2-LICENSE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "usbip\{#UsbipFile}"; DestDir: "{tmp}"; Flags: deleteafterinstall; Components: usbip

[Icons]
Name: "{group}\tapbridge"; Filename: "{app}\tapbridge.exe"

[Registry]
; Start at login (user scope; the app can toggle this from the tray).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "tapbridge"; ValueData: """{app}\tapbridge.exe"""; Flags: uninsdeletevalue

[Run]
; The bundled usbip-win2 installer runs silently, only when the driver is
; missing (its own Inno Setup flags), before the app is offered to start.
Filename: "{tmp}\{#UsbipFile}"; Parameters: "/VERYSILENT /NORESTART /SUPPRESSMSGBOXES"; StatusMsg: "Installing the USB/IP driver..."; Check: NeedsUsbip; Components: usbip
Filename: "{app}\tapbridge.exe"; Description: "Start tapbridge"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; state stays in %LOCALAPPDATA%\tapbridge by design (pairing survives reinstall)

[Code]
function NeedsUsbip(): Boolean;
begin
  Result := not FileExists(ExpandConstant('{pf}\USBip\usbip.exe'));
  if not Result then
    Log('usbip-win2 already installed; skipping bundled driver installer');
end;

// Stopping the app is enough: usbip-win2 releases our port as soon as the
// app's USB/IP connection closes. Never detach by port number here; port 1
// may belong to another device.
procedure StopApp();
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM tapbridge.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

// Upgrades: the running exe is locked, so stop it before files are replaced.
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopApp();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopApp();
  Result := True;
end;
