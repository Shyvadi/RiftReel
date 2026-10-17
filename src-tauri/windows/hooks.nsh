; NSIS hooks for RiftReel's installer (bundle.windows.nsis.installerHooks).
;
; RiftReel installs to %LOCALAPPDATA%\Programs\RiftReel, where tools/install-local.ps1
; puts it too. Tauri's per-user default, %LOCALAPPDATA%\RiftReel, is RiftReel's data
; folder (the match library, logs), which the program shouldn't share. A folder the user
; picks on the directory page is kept as it is. This file is included before the
; template defines PRODUCTNAME, so the name is spelled out.

; Before the first page shows, so the directory page offers the right folder.
!define MUI_CUSTOMFUNCTION_GUIINIT RiftReelInstallDir

Function RiftReelInstallDir
  ${If} $INSTDIR == "$LOCALAPPDATA\RiftReel"
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\RiftReel"
  ${EndIf}
FunctionEnd

; A silent install never shows a page: move the default here instead.
!macro NSIS_HOOK_PREINSTALL
  ${If} $INSTDIR == "$LOCALAPPDATA\RiftReel"
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\RiftReel"
    SetOutPath $INSTDIR
  ${EndIf}
!macroend

; Starting with Windows ([startup] with_windows) is a Run value pointing at the program,
; which is gone now. A reinstall puts it back when RiftReel next starts.
!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "RiftReel"
!macroend
