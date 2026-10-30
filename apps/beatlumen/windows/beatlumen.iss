; Inno Setup script for the Windows installer (Beatlumen-Setup-x64.exe).
; Installs for the current user (no administrator rights needed) from the
; folder prepared by the CI (dist/windows: beatlumen.exe, Qt libraries,
; Visual C++ runtime, beatlumen-server.exe).
;
;   iscc /DAppVersion=0.10.0 /DSourceDir=..\..\..\dist\windows /DOutputDir=..\..\..\dist apps\beatlumen\windows\beatlumen.iss
;
; SPDX-License-Identifier: AGPL-3.0-or-later

#ifndef AppVersion
  #define AppVersion "0.10.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\..\dist\windows"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\..\dist"
#endif

[Setup]
AppId={{6F0C5E37-4B8E-4C59-9C3A-2B7C9D1E5A41}
AppName=Beatlumen
AppVersion={#AppVersion}
AppPublisher=Beatlumen
AppPublisherURL=https://github.com/RandomNam3/Beatlumen
AppSupportURL=https://github.com/RandomNam3/Beatlumen/issues
DefaultDirName={autopf}\Beatlumen
DefaultGroupName=Beatlumen
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\..\..\LICENSE
OutputDir={#OutputDir}
OutputBaseFilename=Beatlumen-Setup-x64
SetupIconFile=beatlumen.ico
UninstallDisplayIcon={app}\beatlumen.exe
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\..\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Beatlumen"; Filename: "{app}\beatlumen.exe"
Name: "{autodesktop}\Beatlumen"; Filename: "{app}\beatlumen.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\beatlumen.exe"; Description: "{cm:LaunchProgram,Beatlumen}"; Flags: nowait postinstall skipifsilent
