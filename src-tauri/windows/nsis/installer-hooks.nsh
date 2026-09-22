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
