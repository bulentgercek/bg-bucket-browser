# Changelog

## 1.2.0 - 2026-09-28

Security hardening after an independent review of the 1.1.0 source code, and
a round of interface improvements. Released under the Apache License 2.0, as
before.

### Upgrading from 1.1.x

- A connection whose endpoint uses plain `http://` now works only for
  `localhost`, `127.0.0.1` and `[::1]`; any other address needs `https://`.
  An existing connection of that kind stops with a message that points to
  Settings. To reach a server without HTTPS, forward its port to `localhost`,
  for example with an SSH tunnel.
- An upload or a "Download as ZIP" left unfinished by 1.1.x starts again from
  the beginning.

### Security

- Names that come from a volume are checked before anything is written
  locally: a download, an opened file or a ZIP archive can no longer place a
  file outside the folder you chose. On Windows, names the system cannot store
  safely (such as `CON`, or names containing `\` or `:`) are refused.
- Uploading or moving a local folder no longer follows symbolic links or
  junctions inside it and skips names that are not valid UTF-8; what was
  skipped is reported when the job ends. The app's own resume files are never
  written through a link.
- Saved keys are sent only to the endpoint they were saved for: changing a
  connection's endpoint asks for its keys again, and Test uses only the keys in
  the form. The RunPod API key can now be removed, and every removal from the
  credential store is confirmed by reading it back.
- A transfer stays on the connection it was started on, even if you switch
  connections while it is being prepared. A connection's endpoint or bucket
  cannot be changed while it has transfers queued or running.
- The app no longer aborts unfinished uploads on the volume at startup, which
  could include uploads made by other tools. They are listed in Settings (Scan
  for interrupted uploads) and aborted only when you confirm.
- Downloaded and opened files carry the operating system's "from the
  internet" mark (Zone.Identifier on Windows, quarantine on macOS), so
  SmartScreen and Gatekeeper check them as they would a browser download.
- The macOS Finder service opens the app by its bundle identifier.
- Copy, move, rename and Volume Cleanup are tied to the version of an object
  they started from: an object that changes in the meantime is skipped and
  reported, and a move deletes only the version it sent. RunPod honors these
  conditions; some other S3 servers ignore them for deletes.
- Upload resume is tied to the file itself: its size, modification time and
  creation time. A file that changed is uploaded again from the start and its
  earlier unfinished upload is removed from the volume; a move never deletes a
  file that was replaced after it was sent.
- "Download as ZIP" writes its archives itself. Archives larger than 4 GiB are
  no longer damaged when a download resumes, as they were in 1.1.x, and
  resuming checks each file's name, size and ETag.
- Remote listings stop when you navigate away, and a folder listing stops at
  one million objects (five million for a folder tree). Preparing a transfer
  can be cancelled, closing Properties stops its size scan, and Volume Cleanup
  keeps only what its report shows.
- Server responses are read only up to the size expected, and thumbnail
  generation is bounded: images above 100 megapixels are skipped.
- The OS clipboard is read one request at a time, and on Wayland within a time
  and size limit; a clipboard that cannot be read no longer clears an in-app
  copy.

### Interface

- Four font sizes (Small, Medium, Large, Extra Large). Outside the panels,
  Ctrl + / - and Ctrl + mouse wheel step through them (Cmd on macOS); inside a
  panel they change its row and tile size.
- Folders take their pane's color, lilac for a volume and amber for the local
  disk, and icons are filled. Each pane shows its selection in its own color.
- A startup screen shows the logo instead of an empty window, and the theme
  and font size are in place from the first frame.
- Large folders stay responsive: only the rows and tiles near the screen are
  drawn, and switching the theme no longer blanks a long list for a moment.
- The Icons view works with the keyboard like the Details view (arrows, Page
  Up/Down, Shift + arrows to select), and a selected tile is clearly marked.
- In a short window, Settings keeps its top bar in place; only the content
  scrolls.

### Fixes

- On Windows, F5 and Ctrl+R no longer reload the whole interface while a
  dialog or Settings is open.
- The error notification of a job that fails at once now names its file.
- Dragging quickly from one pane to the other and stopping no longer leaves
  "Can't drop here" on screen.

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
