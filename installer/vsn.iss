; Visual Studio Code Native's Windows installer, after VS Code's own Inno Setup
; one: a per-user install by default (no admin), with the same optional
; Explorer menu entries and PATH. Build it with installer\build.ps1.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z (installer\build.ps1 does)
#endif

#define AppName "Visual Studio Code Native"
#define AppExe "vsn.exe"

[Setup]
AppId={{CB6B6BB0-CA2A-45E8-903E-2828543A0923}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=HTRMC
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Per-user like VS Code's User Setup; the dialog offers all users too.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\target\installer
OutputBaseFilename=VSNUserSetup-x64-{#AppVersion}
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; Tells Explorer about the PATH and menu changes.
ChangesEnvironment=yes
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addcontextmenufiles"; Description: "Add ""Open with {#AppName}"" action to Windows Explorer file context menu"; GroupDescription: "Other:"; Flags: unchecked
Name: "addcontextmenufolders"; Description: "Add ""Open with {#AppName}"" action to Windows Explorer directory context menu"; GroupDescription: "Other:"; Flags: unchecked
Name: "addtopath"; Description: "Add to PATH (requires shell restart)"; GroupDescription: "Other:"

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "vsn.cmd"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "..\target\release\runtime\node.exe"; DestDir: "{app}\runtime"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\assets\THIRD_PARTY.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; HKA is HKCU\Software\Classes for a per-user install, HKLM for all users.
Root: HKA; Subkey: "Software\Classes\*\shell\vsn"; ValueType: expandsz; ValueName: ""; ValueData: "Open w&ith {#AppName}"; Tasks: addcontextmenufiles; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\*\shell\vsn"; ValueType: expandsz; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"; Tasks: addcontextmenufiles
Root: HKA; Subkey: "Software\Classes\*\shell\vsn\command"; ValueType: expandsz; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: addcontextmenufiles
Root: HKA; Subkey: "Software\Classes\directory\shell\vsn"; ValueType: expandsz; ValueName: ""; ValueData: "Open w&ith {#AppName}"; Tasks: addcontextmenufolders; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\directory\shell\vsn"; ValueType: expandsz; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"; Tasks: addcontextmenufolders
Root: HKA; Subkey: "Software\Classes\directory\shell\vsn\command"; ValueType: expandsz; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%V"""; Tasks: addcontextmenufolders
Root: HKA; Subkey: "Software\Classes\directory\background\shell\vsn"; ValueType: expandsz; ValueName: ""; ValueData: "Open w&ith {#AppName}"; Tasks: addcontextmenufolders; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\directory\background\shell\vsn"; ValueType: expandsz; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"; Tasks: addcontextmenufolders
Root: HKA; Subkey: "Software\Classes\directory\background\shell\vsn\command"; ValueType: expandsz; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%V"""; Tasks: addcontextmenufolders
; PATH gets {app}\bin, whose vsn.cmd is the only thing in it.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}\bin"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}\bin')) and not IsAdminInstallMode
Root: HKLM; Subkey: "SYSTEM\CurrentControlSet\Control\Session Manager\Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}\bin"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}\bin')) and IsAdminInstallMode

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; vsn's Update Now runs this setup silently with /update=1 once it has quit;
; start it again, as VS Code's update does.
Filename: "{app}\{#AppExe}"; Flags: nowait runasoriginaluser; Check: IsUpdate

[Code]
function IsUpdate: Boolean;
begin
  Result := ExpandConstant('{param:update|0}') = '1';
end;

function PathKey(var Root: Integer): String;
begin
  if IsAdminInstallMode then begin
    Root := HKEY_LOCAL_MACHINE;
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  end else begin
    Root := HKEY_CURRENT_USER;
    Result := 'Environment';
  end;
end;

function NeedsAddPath(Dir: String): Boolean;
var
  Root: Integer;
  Path: String;
begin
  if not RegQueryStringValue(Root, PathKey(Root), 'Path', Path) then begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

// Uninstalling takes {app}\bin back out of PATH.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Root, At: Integer;
  Key, Path, Dir: String;
begin
  if CurUninstallStep <> usUninstall then
    exit;
  Key := PathKey(Root);
  if not RegQueryStringValue(Root, Key, 'Path', Path) then
    exit;
  Dir := ';' + ExpandConstant('{app}\bin');
  At := Pos(Uppercase(Dir), Uppercase(Path));
  if At > 0 then begin
    Delete(Path, At, Length(Dir));
    RegWriteExpandStringValue(Root, Key, 'Path', Path);
  end;
end;
