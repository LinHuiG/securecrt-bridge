; Uses the same NSIS / Modern UI 2 toolchain as ../shishi/installer/setup.nsi.
Unicode true
!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "x64.nsh"
Name "SecureCRT Bridge"
OutFile "${OUTPUT}\SecureCRTBridge-${VERSION}-Setup.exe"
InstallDir "$LOCALAPPDATA\Programs\SecureCRTBridge"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show
BrandingText "SecureCRT Bridge ${VERSION}"
VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=2052 "ProductName" "SecureCRT Bridge"
VIAddVersionKey /LANG=2052 "FileDescription" "SecureCRT Bridge 安装程序"
VIAddVersionKey /LANG=2052 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=2052 "LegalCopyright" "Copyright (c) 2026 LinHuiG; AGPL-3.0-only"
Var Plan
Var Existing
Var ExistingDir
Var Dialog
Var RemoveRadio
Var Crt
Var Config
Var CrtEdit
Var ConfigEdit
Var ExtraPaths
Var ExtraEdit
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "欢迎安装 SecureCRT Bridge"
!define MUI_FINISHPAGE_TITLE "SecureCRT Bridge 安装完成"
!define MUI_WELCOMEPAGE_TEXT "此向导将引导您安装 SecureCRT Bridge。$\r$\n$\r$\n让 AI 使用已登录的 SecureCRT 标签页，命令与输出保留在原终端中。$\r$\n$\r$\n安装前请关闭 SecureCRT；安装器将备份原配置。"
!define MUI_FINISHPAGE_TEXT "SecureCRT Bridge 已安装，本地服务已启动。$\r$\n$\r$\n现在可以打开 SecureCRT，或从堡垒机连接 SSH。所选 AI 客户端的 Skill 已安装。$\r$\n$\r$\n程序可在 Windows“已安装的应用”中卸载，原配置备份会保留。"
!insertmacro MUI_PAGE_WELCOME
Page custom Maintenance MaintenanceLeave
!insertmacro MUI_PAGE_DIRECTORY
Page custom Terminal TerminalLeave
Page custom Clients ClientsLeave
Page custom Extra ExtraLeave
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH
!insertmacro MUI_LANGUAGE "SimpChinese"

!macro ClientVars N
Var Check${N}
Var Edit${N}
!macroend
!insertmacro ClientVars 0
!insertmacro ClientVars 1
!insertmacro ClientVars 2
!insertmacro ClientVars 3
!insertmacro ClientVars 4
!insertmacro ClientVars 5
!insertmacro ClientVars 6

Function .onInit
 ${IfNot} ${RunningX64}
  MessageBox MB_ICONSTOP "需要 64 位 Windows 和 SecureCRT。" /SD IDOK
  Abort
 ${EndIf}
 SetRegView 64
 InitPluginsDir
 SetOutPath "$PLUGINSDIR"
 File /oname=bridge.exe "${PAYLOAD}"
 StrCpy $Plan "$PLUGINSDIR\setup.ini"
 nsExec::ExecToStack /TIMEOUT=15000 '"$PLUGINSDIR\bridge.exe" setup-export --file "$Plan" --nsis'
 Pop $0
 Pop $1
 ${If} $0 != 0
  MessageBox MB_ICONSTOP "无法读取安装信息：$\r$\n$1" /SD IDOK
  Abort
 ${EndIf}
 ReadINIStr $Existing $Plan "setup" "existing"
 ReadINIStr $ExistingDir $Plan "setup" "directory"
 ${If} $Existing != ""
  StrCpy $INSTDIR $ExistingDir
 ${EndIf}
 ReadINIStr $Crt $Plan "setup" "securecrt"
 ReadINIStr $Config $Plan "setup" "config"
 WriteUninstaller "$PLUGINSDIR\Uninstall.exe"
 WriteINIStr $Plan "setup" "uninstaller" "$PLUGINSDIR\Uninstall.exe"
 ; Preserve all previously selected custom client paths on the additional page.
 StrCpy $ExtraPaths ""
 ReadINIStr $2 $Plan "setup" "clients"
 StrCpy $3 7
 ${While} $3 < $2
  ReadINIStr $4 $Plan "client$3" "selected"
  ${If} $4 == 1
   ReadINIStr $4 $Plan "client$3" "path"
   StrCpy $ExtraPaths "$ExtraPaths$4$\r$\n"
  ${EndIf}
  IntOp $3 $3 + 1
 ${EndWhile}
 ; /S must use the same complete plan even though page callbacks are skipped.
 Call SaveExtra
FunctionEnd

Function Maintenance
 ${If} $Existing == ""
  Abort
 ${EndIf}
 !insertmacro MUI_HEADER_TEXT "已安装 SecureCRT Bridge" "选择升级或卸载已有版本。"
 nsDialogs::Create 1018
 Pop $Dialog
 ${NSD_CreateLabel} 0 0 100% 42u "当前版本：$Existing$\r$\n安装位置：$ExistingDir"
 Pop $0
 ${NSD_CreateRadioButton} 0 55u 100% 20u "升级（保留原配置备份与暂停状态）"
 Pop $0
 ${NSD_Check} $0
 ${NSD_CreateRadioButton} 0 85u 100% 20u "卸载（恢复原配置，保留用户修改与备份）"
 Pop $RemoveRadio
 nsDialogs::Show
FunctionEnd
Function MaintenanceLeave
 ${NSD_GetState} $RemoveRadio $0
 ${If} $0 == ${BST_CHECKED}
  ; NSIS copies itself to temp before invoking the bridge cleanup backend.
  ExecWait '"$PLUGINSDIR\Uninstall.exe" _?=$ExistingDir' $0
  ${If} $0 != 0
   MessageBox MB_ICONEXCLAMATION "卸载未完成，请查看卸载窗口中的信息。"
   Abort
  ${EndIf}
  Quit
 ${EndIf}
FunctionEnd

Function Terminal
 !insertmacro MUI_HEADER_TEXT "连接 SecureCRT" "选择已有的 SecureCRT 程序及其配置目录。"
 nsDialogs::Create 1018
 Pop $Dialog
 ${NSD_CreateLabel} 0 0 100% 18u "SecureCRT.exe 或 SecureCRTPortable.exe"
 Pop $0
 ${NSD_CreateFileRequest} 0 22u 250u 14u "$Crt"
 Pop $CrtEdit
 ${NSD_CreateBrowseButton} 256u 21u 44u 16u "浏览..."
 Pop $0
 ${NSD_OnClick} $0 BrowseCrt
 ${NSD_CreateLabel} 0 57u 100% 18u "配置目录（包含 Sessions\Default.ini）"
 Pop $0
 ${NSD_CreateDirRequest} 0 79u 250u 14u "$Config"
 Pop $ConfigEdit
 ${NSD_CreateBrowseButton} 256u 78u 44u 16u "浏览..."
 Pop $0
 ${NSD_OnClick} $0 BrowseConfig
 ${NSD_CreateLabel} 0 114u 100% 45u "已自动检测现有路径，可通过浏览修改。$\r$\n继续安装前，请先关闭 SecureCRT。"
 Pop $0
 nsDialogs::Show
FunctionEnd
Function BrowseCrt
 Pop $0
 nsDialogs::SelectFileDialog open "$Crt" "SecureCRT 程序|*.exe"
 Pop $0
 ${If} $0 != ""
  ${NSD_SetText} $CrtEdit $0
 ${EndIf}
FunctionEnd
Function BrowseConfig
 Pop $0
 nsDialogs::SelectFolderDialog "选择 SecureCRT 配置目录" "$Config"
 Pop $0
 ${If} $0 != error
  ${NSD_SetText} $ConfigEdit $0
 ${EndIf}
FunctionEnd
Function TerminalLeave
 nsExec::ExecToStack /TIMEOUT=15000 '"$PLUGINSDIR\bridge.exe" setup-check --nsis'
 Pop $0
 Pop $1
 ${If} $0 != 0
  MessageBox MB_ICONEXCLAMATION "$1"
  Abort
 ${EndIf}
 ${NSD_GetText} $CrtEdit $Crt
 ${NSD_GetText} $ConfigEdit $Config
 IfFileExists "$Crt" +3 0
 MessageBox MB_ICONEXCLAMATION "请选择有效的 SecureCRT 程序。"
 Abort
 IfFileExists "$Config\Sessions\Default.ini" +3 0
 MessageBox MB_ICONEXCLAMATION "配置目录中未找到 Sessions\Default.ini。"
 Abort
 WriteINIStr $Plan "setup" "securecrt" "$Crt"
 WriteINIStr $Plan "setup" "config" "$Config"
FunctionEnd

!macro ClientRow N Y
 ReadINIStr $0 $Plan "client${N}" "name"
 ${NSD_CreateCheckbox} 0 ${Y}u 103u 15u "$0"
 Pop $Check${N}
 ReadINIStr $0 $Plan "client${N}" "selected"
 ${NSD_SetState} $Check${N} $0
 ReadINIStr $0 $Plan "client${N}" "path"
 ${NSD_CreateDirRequest} 106u ${Y}u 161u 14u "$0"
 Pop $Edit${N}
 ${NSD_CreateBrowseButton} 272u ${Y}u 28u 14u "..."
 Pop $0
 ${NSD_OnClick} $0 Browse${N}
!macroend
!macro ClientSave N
 ${NSD_GetState} $Check${N} $0
 WriteINIStr $Plan "client${N}" "selected" "$0"
 ${NSD_GetText} $Edit${N} $0
 WriteINIStr $Plan "client${N}" "path" "$0"
!macroend
!macro ClientBrowse N
Function Browse${N}
 Pop $0
 ${NSD_GetText} $Edit${N} $0
 nsDialogs::SelectFolderDialog "选择 Skill 的 securecrt-bridge 子目录" "$0"
 Pop $0
 ${If} $0 != error
  ${NSD_SetText} $Edit${N} $0
 ${EndIf}
FunctionEnd
!macroend
!insertmacro ClientBrowse 0
!insertmacro ClientBrowse 1
!insertmacro ClientBrowse 2
!insertmacro ClientBrowse 3
!insertmacro ClientBrowse 4
!insertmacro ClientBrowse 5
!insertmacro ClientBrowse 6
Function Clients
 !insertmacro MUI_HEADER_TEXT "安装 AI Skill" "已检测到的客户端默认选中，也可以全部取消。"
 nsDialogs::Create 1018
 Pop $Dialog
 ${NSD_CreateLabel} 0 0 100% 16u "客户端                         Skill 安装目录（可修改）"
 Pop $0
 !insertmacro ClientRow 0 23
 !insertmacro ClientRow 1 43
 !insertmacro ClientRow 2 63
 !insertmacro ClientRow 3 83
 !insertmacro ClientRow 4 103
 !insertmacro ClientRow 5 123
 !insertmacro ClientRow 6 143
 nsDialogs::Show
FunctionEnd
Function ClientsLeave
 !insertmacro ClientSave 0
 !insertmacro ClientSave 1
 !insertmacro ClientSave 2
 !insertmacro ClientSave 3
 !insertmacro ClientSave 4
 !insertmacro ClientSave 5
 !insertmacro ClientSave 6
FunctionEnd
Function Extra
 !insertmacro MUI_HEADER_TEXT "其他客户端（可选）" "每行一个 Skill 子目录；无需额外目录时留空。"
 nsDialogs::Create 1018
 Pop $Dialog
 nsDialogs::CreateControl EDIT ${DEFAULT_STYLES}|${WS_VSCROLL}|${WS_TABSTOP}|${ES_MULTILINE}|${ES_WANTRETURN}|${ES_AUTOVSCROLL} ${WS_EX_CLIENTEDGE} 0 0 100% 112u "$ExtraPaths"
 Pop $ExtraEdit
 ${NSD_CreateBrowseButton} 0 122u 70u 18u "添加目录..."
 Pop $0
 ${NSD_OnClick} $0 BrowseExtra
 ${NSD_CreateLabel} 0 149u 100% 24u "点击“安装”前会检查路径、已有文件及 SecureCRT 状态。"
 Pop $0
 nsDialogs::Show
FunctionEnd
Function BrowseExtra
 Pop $0
 nsDialogs::SelectFolderDialog "选择客户端的 skills 父目录" ""
 Pop $0
 ${If} $0 != error
  ${NSD_GetText} $ExtraEdit $ExtraPaths
  ${NSD_SetText} $ExtraEdit "$ExtraPaths$0\securecrt-bridge$\r$\n"
 ${EndIf}
FunctionEnd
Function SaveExtra
 FileOpen $0 "$PLUGINSDIR\extra.txt" w
 FileWriteWord $0 0xFEFF
 FileWriteUTF16LE $0 "$ExtraPaths"
 FileClose $0
 WriteINIStr $Plan "setup" "clients" "7"
 WriteINIStr $Plan "setup" "extra_file" "$PLUGINSDIR\extra.txt"
 WriteINIStr $Plan "setup" "directory" "$INSTDIR"
FunctionEnd
Function ExtraLeave
 ${NSD_GetText} $ExtraEdit $ExtraPaths
 Call SaveExtra
 nsExec::ExecToStack /TIMEOUT=15000 '"$PLUGINSDIR\bridge.exe" setup-validate --file "$Plan" --nsis'
 Pop $0
 Pop $1
 ${If} $0 != 0
  MessageBox MB_ICONEXCLAMATION "安装前检查未通过：$\r$\n$1"
  Abort
 ${EndIf}
FunctionEnd
Section "SecureCRT Bridge" SEC_MAIN
 SectionIn RO
 AddSize 7000
 Call SaveExtra
 nsExec::ExecToLog '"$PLUGINSDIR\bridge.exe" setup-apply --file "$Plan" --nsis'
 Pop $0
 ${If} $0 != 0
  DetailPrint "安装未完成，请查看上方错误。"
  SetErrorLevel 1
  Abort
 ${EndIf}
 SetErrorLevel 0
SectionEnd
Function un.onInit
 SetRegView 64
 InitPluginsDir
 SetOutPath "$PLUGINSDIR"
 File /oname=bridge.exe "${PAYLOAD}"
FunctionEnd
Section "Uninstall"
 ; The journal restores only unchanged installed values and preserves user edits.
 nsExec::ExecToLog '"$PLUGINSDIR\bridge.exe" uninstall --noninteractive --expect-dir "$INSTDIR" --nsis'
 Pop $0
 ${If} $0 != 0
  DetailPrint "卸载未完成，请查看上方错误。"
  SetErrorLevel 1
  Abort
 ${EndIf}
 SetErrorLevel 0
SectionEnd
