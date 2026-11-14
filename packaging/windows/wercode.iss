; The Windows installer. Built by the release workflow:
;   ISCC.exe /DVersion=0.1.0 packaging\windows\wercode.iss
; It installs for the current user only (no administrator rights), into
; %LOCALAPPDATA%\Programs\Wercode, so Wercode can update itself: it downloads
; a newer installer and runs it with /VERYSILENT as it quits.

#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
AppId={{7D3F1B52-6C4E-4A8B-9E21-5F0C8D2A4B17}
AppName=Wercode
AppVersion={#Version}
AppVerName=Wercode {#Version}
AppPublisher=Wercode
AppPublisherURL=https://github.com/Werroks/Wercode
AppSupportURL=https://github.com/Werroks/Wercode/issues
AppUpdatesURL=https://github.com/Werroks/Wercode/releases
DefaultDirName={localappdata}\Programs\Wercode
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\..\dist
OutputBaseFilename=Wercode-Setup-{#Version}-x64
SetupIconFile=..\..\assets\brand\wercode.ico
UninstallDisplayIcon={app}\wercode.exe
UninstallDisplayName=Wercode
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=force
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"

[CustomMessages]
english.FolderMenu=Add "Open with Wercode" to the menu of folders
russian.FolderMenu=Добавить «Открыть в Wercode» в меню папок
german.FolderMenu=„Mit Wercode öffnen“ zum Kontextmenü von Ordnern hinzufügen
spanish.FolderMenu=Añadir «Abrir con Wercode» al menú de las carpetas
english.OpenWith=Open with Wercode
russian.OpenWith=Открыть в Wercode
german.OpenWith=Mit Wercode öffnen
spanish.OpenWith=Abrir con Wercode

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "foldermenu"; Description: "{cm:FolderMenu}"

[Files]
Source: "..\..\target\release\wercode.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Wercode"; Filename: "{app}\wercode.exe"
Name: "{autodesktop}\Wercode"; Filename: "{app}\wercode.exe"; Tasks: desktopicon

[Registry]
; A folder's own menu, and the menu of the empty space inside a folder.
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Wercode"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWith}"; Flags: uninsdeletekey; Tasks: foldermenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Wercode"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\wercode.exe"""; Tasks: foldermenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Wercode\command"; ValueType: string; ValueName: ""; ValueData: """{app}\wercode.exe"" ""%V"""; Tasks: foldermenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Wercode"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWith}"; Flags: uninsdeletekey; Tasks: foldermenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Wercode"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\wercode.exe"""; Tasks: foldermenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Wercode\command"; ValueType: string; ValueName: ""; ValueData: """{app}\wercode.exe"" ""%V"""; Tasks: foldermenu

[Run]
; After an interactive install: the usual "Launch" checkbox. After an update
; Wercode started on its way out: only when it asked to be started again.
Filename: "{app}\wercode.exe"; Description: "{cm:LaunchProgram,Wercode}"; Flags: nowait postinstall; Check: ShouldLaunch

[Code]
function ShouldLaunch: Boolean;
begin
  Result := (not WizardSilent) or (ExpandConstant('{param:relaunch|0}') = '1');
end;

function InitializeSetup: Boolean;
var
  Waited: Integer;
begin
  { An update is started by Wercode just before it quits. Give it a few
    seconds to go before files are replaced. }
  if WizardSilent then
  begin
    Waited := 0;
    while CheckForMutexes('WercodeRunning') and (Waited < 75) do
    begin
      Sleep(200);
      Waited := Waited + 1;
    end;
  end;
  Result := True;
end;
