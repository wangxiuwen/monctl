; Inno Setup script: monctl-setup.exe
; Per-user install (no admin prompt) into %LOCALAPPDATA%\Programs\monctl.
;   iscc /DVersion=0.2.0 /DSrc=..\..\target\release packaging\windows\monctl.iss

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Src
  #define Src "..\..\target\release"
#endif

[Setup]
AppId={{7C1E3A52-8E1B-4C55-9B57-6D6F6E63746C}
AppName=monctl
AppVersion={#Version}
AppPublisher=monctl
AppPublisherURL=https://github.com/wangxiuwen/monctl
DefaultDirName={localappdata}\Programs\monctl
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\..\dist
OutputBaseFilename=monctl-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesEnvironment=yes
UninstallDisplayIcon={app}\monctl-tray.exe
WizardStyle=modern
CloseApplications=yes

[Tasks]
Name: "autostart"; Description: "Start the tray when I sign in / 登录时自动启动托盘"
Name: "addpath"; Description: "Add monctl to PATH / 把 monctl 加入 PATH（任意终端可用）"
Name: "screendriver"; Description: "Install the screen driver for tablets as extra screens (asks for admin once) / 安装平板扩展屏用的屏幕驱动（需要管理员授权一次）"

[Files]
Source: "{#Src}\monctl.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Src}\monctl-tray.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\monctl"; Filename: "{app}\monctl-tray.exe"

[Registry]
; same Run value the tray's own 开机启动 toggle uses, so both stay in sync
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "monctl-tray"; ValueData: """{app}\monctl-tray.exe"""; Flags: uninsdeletevalue; Tasks: autostart
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addpath; Check: NeedsPath(ExpandConstant('{app}'))

[Run]
; the open-source Virtual Display Driver (signed, embedded in monctl.exe): done now so
; the first "投屏到平板" just works - otherwise monctl asks for it on first use
Filename: "{app}\monctl.exe"; Parameters: "driver install"; Verb: "runas"; Flags: shellexec runhidden waituntilterminated; StatusMsg: "Installing the screen driver..."; Tasks: screendriver
Filename: "{app}\monctl-tray.exe"; Description: "Start monctl now / 立即启动 monctl 托盘"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM monctl-tray.exe"; Flags: runhidden; RunOnceId: "StopTray"

[Code]
function NeedsPath(Dir: string): Boolean;
var
  Paths: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Paths) then
    Paths := '';
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Paths, Dir: string;
  P, Code: Integer;
begin
  // the screen driver (if it was installed) goes too - while monctl.exe still exists
  if (CurUninstallStep = usUninstall) and DirExists(ExpandConstant('{commonappdata}\monctl\vdd')) then
    ShellExec('runas', ExpandConstant('{app}\monctl.exe'), 'driver uninstall', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if CurUninstallStep <> usPostUninstall then
    exit;
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Paths) then
    exit;
  Dir := ExpandConstant('{app}');
  P := Pos(';' + Uppercase(Dir), Uppercase(Paths));
  if P > 0 then
  begin
    Delete(Paths, P, Length(Dir) + 1);
    RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Paths);
  end;
end;
