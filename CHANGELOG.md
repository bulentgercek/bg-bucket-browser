# Changelog

## 1.0.0 - 2026-09-23

First release. Released under the Apache License 2.0.

### The application

- Dual-pane file manager for RunPod network volumes: a volume on one side,
  the local filesystem on the other, either pane holding either source. No
  running pod is needed.
- Multiple named volume connections, switchable from the sidebar. Pinned
  locations belong to their connection, and each connection reopens at the
  folder it was left in.
- Transfers in all four directions, folders included: local to local, up,
  down, and volume to volume. `F5` copies the active pane's selection to the
  other pane, `F6` moves it.
- Downloads and multipart uploads resume: an interrupted transfer continues
  from where it stopped. A download is checked against the object's ETag, so
  an object that changes mid-transfer fails instead of arriving half new.
- Download several files or folders straight into one zip archive.
- Drag and drop between panes, tabs, pinned locations and recent paths on all
  three platforms; dragging files in from the OS file manager works on Linux.
- Copy, cut and paste that interoperate with the desktop file manager's own
  clipboard, verified with Dolphin and Nautilus, Explorer, and Finder.
- Image and video thumbnails, generated from partial reads so a large remote
  file is not downloaded in full just to show a preview.
- Volume cleanup: scans the volume, reports the largest folders and objects
  and the reclaimable ones (`__pycache__`, `.pyc`, `.ipynb_checkpoints`,
  zero-byte files), and deletes what you select.
- Total volume capacity in the status bar, read from the RunPod account API.
- "Open in BG Bucket Browser" in the OS file manager's context menu, switched
  on and off from Settings and cleaned up by the packaged uninstallers.
- Light, dark and system theme.

### Security and privacy

- Credentials live in the operating system's own store — Secret Service on
  Linux, Credential Manager on Windows, Keychain on macOS — and are never
  written to disk in plain text. There is no plaintext fallback: a missing
  credential store is an error, not a silent downgrade.
- The packaged application runs under a content security policy that allows
  only its own scripts, styles and fonts.
- Names that would write outside the folder you chose are refused, both when
  creating a folder on a volume and when a downloaded object's name would
  climb out of the destination.

### Platforms

- Linux (AppImage, `.deb`), Windows (`.msi`, NSIS `.exe`), macOS (`.dmg`).
  Each package is built on its own operating system; there is no
  cross-compilation. Every platform was installed from its own package and
  tested end to end.
