; Removes the folder context menu entry when the app is uninstalled.
;
; That entry is written by the app itself, from its own settings, so the
; installer never created it and would not clean it up: a user who leaves the
; setting on would keep the registry key forever.
;
; Deleting a key that is not there does nothing, so this is safe whether the
; setting was ever used or not.
!macro NSIS_HOOK_PREUNINSTALL
  DeleteRegKey HKCU "Software\Classes\Directory\shell\BGBucketBrowserOpenHere"
!macroend

; Removes the stored keys from Credential Manager.
;
; The keys are not in the application data folder: they live in the user's
; credential store, each under a name that ends in the app's service name. A
; user who ticks "delete the application data" means those as well, and with
; the connection list gone nothing in the app could remove them afterwards.
;
; Only with that box ticked, and never on an update: an update runs this
; uninstaller too, and would take the keys away each time.
!define BGBB_KEY_SUFFIX ".bg-bucket-browser"
!define BGBB_KEY_SUFFIX_LEN 18
!macro BGBB_DELETE_STORED_KEYS
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  Push $6
  Push $7
  System::Call 'advapi32::CredEnumerateW(p 0, i 0, *i .r1, *p .r2) i .r0'
  ${If} $0 <> 0
    StrCpy $3 0
    ${While} $3 < $1
      IntOp $4 $3 * ${NSIS_PTR_SIZE}
      IntPtrOp $4 $2 + $4
      System::Call '*$4(p .r5)'
      ; CREDENTIALW starts with Flags, Type, TargetName.
      System::Call '*$5(i, i .r6, w .r7)'
      StrCpy $4 $7 "" -${BGBB_KEY_SUFFIX_LEN}
      ${If} $4 == "${BGBB_KEY_SUFFIX}"
        System::Call 'advapi32::CredDeleteW(w r7, i r6, i 0)'
      ${EndIf}
      IntOp $3 $3 + 1
    ${EndWhile}
    System::Call 'advapi32::CredFree(p r2)'
  ${EndIf}
  Pop $7
  Pop $6
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
!macroend

; Forgets the install location when the app is uninstalled.
;
; The installer keeps it in the registry so an update lands in the same folder,
; and the uninstaller removes it only when "delete the application data" is
; ticked. Left behind, it is where the .msi package installs next, instead of
; Program Files: the two packages look the previous location up in the same
; place.
;
; An update runs this uninstaller too, and must find the location afterwards.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue SHCTX "${MANUPRODUCTKEY}" ""
    DeleteRegKey /ifempty SHCTX "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty SHCTX "${MANUKEY}"
  ${EndIf}
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    !insertmacro BGBB_DELETE_STORED_KEYS
  ${EndIf}
!macroend
