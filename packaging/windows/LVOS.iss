#ifndef SourceRoot
  #error SourceRoot must point to the LVOS repository root
#endif
#ifndef MyAppVersion
  #error MyAppVersion must be supplied by the packaging script
#endif

#define AgentBinary SourceRoot + "\target\release\lvos-agent.exe"
#define UiBinary SourceRoot + "\target\release\lvos-ui.exe"

[Setup]
AppId={{5D741825-65C6-4B58-9CEB-D9F24B2326C7}
AppName=LVOS
AppVersion={#MyAppVersion}
AppPublisher=wadaxiyang
AppPublisherURL=https://github.com/wadaxiyang/LVOS
AppSupportURL=https://github.com/wadaxiyang/LVOS/issues
AppUpdatesURL=https://github.com/wadaxiyang/LVOS/releases
DefaultDirName={localappdata}\Programs\LVOS
DefaultGroupName=LVOS
DisableDirPage=no
DisableProgramGroupPage=no
AllowNoIcons=no
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputBaseFilename=LVOS-{#MyAppVersion}-windows-x86_64-setup
SetupIconFile={#SourceRoot}\apps\desktop\resources\lvos.ico
UninstallDisplayIcon={app}\LVOS.exe
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
VersionInfoVersion={#MyAppVersion}.0
VersionInfoCompany=wadaxiyang
VersionInfoDescription=LVOS installer
VersionInfoProductName=LVOS
VersionInfoProductVersion={#MyAppVersion}
LicenseFile={#SourceRoot}\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#AgentBinary}"; DestDir: "{app}"; DestName: "LVOS.exe"; Flags: ignoreversion
Source: "{#UiBinary}"; DestDir: "{app}"; DestName: "lvos-ui.exe"; Flags: ignoreversion
Source: "{#SourceRoot}\LICENSE"; DestDir: "{app}\NOTICES"; DestName: "LVOS-LICENSE.txt"; Flags: ignoreversion
Source: "{#SourceRoot}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}\NOTICES"; Flags: ignoreversion
Source: "{#SourceRoot}\licenses\Quadrant-Kit-GPL-3.0.txt"; DestDir: "{app}\NOTICES"; Flags: ignoreversion
Source: "{#SourceRoot}\licenses\Quadrant-Kit-NOTICES.md"; DestDir: "{app}\NOTICES"; Flags: ignoreversion
Source: "{#SourceRoot}\licenses\Fluent-System-Icons-MIT.txt"; DestDir: "{app}\NOTICES"; Flags: ignoreversion

[Icons]
Name: "{group}\LVOS"; Filename: "{app}\LVOS.exe"; WorkingDir: "{app}"; AppUserModelID: "site.niuniu770.lvos"
Name: "{group}\Uninstall LVOS"; Filename: "{uninstallexe}"
Name: "{autodesktop}\LVOS"; Filename: "{app}\LVOS.exe"; WorkingDir: "{app}"; AppUserModelID: "site.niuniu770.lvos"; Tasks: desktopicon

[Run]
Filename: "{app}\LVOS.exe"; Description: "{cm:LaunchProgram,LVOS}"; Flags: nowait postinstall skipifsilent
