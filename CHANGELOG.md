# Changelog

## 1.1.1 - 2026-09-26

Security update. Released under the Apache License 2.0, as before.

- Updated rustls, the TLS library behind every HTTPS connection the app
  makes (to your volumes, the RunPod API and the feedback service), from
  0.23.44 to 0.23.45. The new version fixes RUSTSEC-2026-0285
  (GHSA-2mjx-qc3c-rqvc): during a TLS 1.3 handshake, some messages were
  accepted at the wrong encryption level. The handshake stayed
  authenticated, so an attacker on the network could not alter or complete
  it. Updating is recommended.
- The repository now has a security policy: `SECURITY.md` explains how to
  report a vulnerability privately.

There are no other changes.

## 1.1.0 - 2026-09-25

Data-safety fixes and in-app feedback. Released under the Apache License 2.0,
as before.

### New: feedback from inside the app

- Settings → Feedback: write a message and send it with one press. No email
  client or GitHub account is needed; an email address for a reply is
  optional.
- **Record the problem** captures a detailed log for up to five minutes while
  you reproduce an issue; a pill in the status bar shows the time and stops
  it. The recording is attached in its own read-only box, so you see exactly
  what will be sent. A recording cut short by a crash is offered again on the
  next start.
- The report goes over HTTPS only when you press Send. The recording contains
  file names and paths, never credentials.

### Fixes: nothing is deleted that was not really moved

- A single-file move no longer deletes its source when the name turns out to
  be taken at the destination while the transfer runs; the item is reported
  as not transferred.
- Copying or moving a folder from volume to volume, and uploading a folder,
  now leave files already at the destination alone, and a move keeps their
  sources.
- Renaming on a volume checks the copy's size before the original is
  removed.
- Cut and paste remember which volume the items came from; pasting while
  another volume is active is refused instead of acting on a same-named file
  there.
- After switching volumes, tabs that were out of view list the new volume
  instead of showing the old one's files.
- Names that begin or end with a space are no longer trimmed, so deleting
  `models ` no longer deletes `models`.
- Download as Zip never overwrites or appends to a `.zip` already in the
  destination; the new archive gets a free "copy" name instead.
- A folder listing that arrives late lands in the tab that asked for it.
- Items hidden by the filter or by the hidden-files setting leave the
  selection, so Delete and the other actions only take what is on screen.

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
