/* Every string the interface shows, in one place.

   The keys are flat and dotted so this file can become plain JSON the day a
   second language is added; `{token}` placeholders are filled in at the call
   site. */

export const en = {
  "app.name": "BG Bucket Browser",

  "main.openSettings": "Open settings",
  "main.toggleHidden": "Toggle hidden files",
  "main.settings": "Settings",
  "main.showHidden": "Show Hidden",
  "main.hideHidden": "Hide Hidden",
  "main.flip": "Swap panels",
  "main.tab.new": "New tab",
  "main.tab.close": "Close tab",
  "main.tab.overflow": "All open tabs",
  "main.connection.connected": "{volume} · connected",
  "main.connection.connecting": "{volume} · connecting…",
  "main.connection.error": "{volume} · unreachable",
  "main.connection.none": "No connection — set one up",

  "main.sidebar.networkVolumes": "Network volumes",
  "main.sidebar.localDevices": "Local devices",
  "main.sidebar.pinned": "Pinned",
  "main.sidebar.recent": "Recent",
  "main.sidebar.legendLocal": "Local",
  "main.sidebar.legendRemote": "Remote",
  "main.sidebar.pinNewTab": "Open in new tab",

  "panel.col.name": "Name",
  "panel.col.size": "Size",
  "panel.col.modified": "Modified",
  "panel.filter": "Filter",
  "panel.filterClear": "Clear filter",
  "panel.sortBy": "Sort by {col}",
  "panel.loading": "Loading…",
  "panel.empty": "This folder is empty",
  "panel.noMatches": "No items match the filter",
  "panel.noConnection": "No connection configured.",
  "panel.credentialError": "This connection is incomplete — check endpoint, keys and bucket.",
  "panel.connectionRejected": "The volume refused this connection — check the keys and the bucket name.",
  "panel.retry": "Retry",
  "panel.openSettings": "Open Settings",
  "panel.crumbMore": "Show hidden path segments",

  // Drag and drop
  "dnd.file": "file",
  "dnd.files": "files",
  "dnd.moveHere": "Move Here",
  "dnd.copyHere": "Copy Here",
  "dnd.cancel": "Cancel",
  "dnd.cantDropHere": "Can't drop here",

  "view.open": "Change view",
  "view.title": "View",
  "view.details": "Details",
  "view.icons": "Icons",
  "view.size": "View size",

  "panel.icons.summary": "{files} files · {folders} folders",

  // Context menu, in the order the design lists
  "ctx.open": "Open",
  "ctx.openInOS": "Open in Local OS",
  "ctx.openNewTab": "Open in New Tab",
  "ctx.addPinned": "Add to Pinned",
  "ctx.newTab": "New Tab",
  "ctx.newTextFile": "New Text File",
  "ctx.refresh": "Refresh",
  "ctx.copyTo": "Copy to {side} panel",
  "ctx.moveTo": "Move to {side} panel",
  "ctx.side.left": "left",
  "ctx.side.right": "right",
  "ctx.download": "Download",
  "ctx.upload": "Upload",
  "ctx.downloadZip": "Download as Zip",
  "ctx.rename": "Rename",
  "ctx.newFolder": "New folder",
  "ctx.copyPath": "Copy S3 path",
  "ctx.properties": "Properties",
  "ctx.delete": "Delete",
  "ctx.deleteTrashTag": "Trash",
  "ctx.deletePermanentTag": "Permanent",
  "ctx.unpin": "Unpin",
  "ctx.selectAll": "Select All",
  "ctx.selectNone": "Select None",
  "ctx.selectInvert": "Invert Selection",
  "ctx.clipCopy": "Copy",
  "ctx.clipMove": "Move",
  "ctx.pasteCopy": "Paste (Copy)",
  "ctx.pasteMove": "Paste (Move)",

  // Creating, renaming, deleting
  "fs.folderCreated": "Folder created: {name}",
  "fs.fileCreated": "File created: {name}",
  "fs.deleted": "Deleted {name}",
  "fs.trashed": "Moved {name} to Trash",
  "fs.nItems": "{count} items",
  "fs.deleting": "Deleting {done}/{total} from {where}…",
  "fs.deletingOne": "Deleting {name}…",
  "fs.deletingFile": "Deleting {name}… 1 file removed",
  "fs.deletingFiles": "Deleting {name}… {files} files removed",
  "fs.renamed": "Renamed to {name}",
  "fs.error.notFound": "Not found",
  "fs.error.accessDenied": "Permission denied",
  "fs.error.alreadyExists": "A file with that name already exists",
  "fs.error.unsupported": "That isn't supported here",
  "fs.error.io": "Could not complete the operation",
  "fs.error.s3": "The server rejected the operation",
  "fs.error.unknown": "Something went wrong",

  // OS entegrasyonu
  "open.failed": "Couldn't open {name}",
  "osdrop.failed": "Couldn't read the dropped items",
  // How a queue row names where a transfer goes
  "route.os": "OS",
  "route.local": "Local",
  "route.remote": "Remote",
  "route.open": "Open",

  "dialog.cancel": "Cancel",
  "dialog.close": "Close",

  "props.name": "Name",
  "props.kind": "Kind",
  "props.kindFolder": "Folder",
  "props.kindFile": "File",
  "props.size": "Size",
  "props.modified": "Modified",
  "props.path": "Path",
  "props.s3uri": "S3 URI",
  "props.calculating": "Calculating…",
  "props.folderSize": "{size} · {count} files",
  "dialog.deleteTitle": "Delete permanently?",
  "dialog.deleteRemoteBody":
    "{name} will be permanently removed — the network volume has no recycle bin. This can't be undone.",
  "dialog.deletePermanentBody":
    "{name} will be permanently deleted, skipping the Trash. This can't be undone.",
  "dialog.deleteRemoteBodyN":
    "{count} items will be permanently removed — the network volume has no recycle bin. This can't be undone.",
  "dialog.deletePermanentBodyN":
    "{count} items will be permanently deleted, skipping the Trash. This can't be undone.",

  "dialog.conflictTitle": "Name conflict",
  "dialog.conflictBody": "“{name}” already exists in “{dest}”.",
  "dialog.conflictApplyAll": "Apply to all {count} conflicts",
  "dialog.conflictSkip": "Skip",
  "dialog.conflictRename": "Rename",
  "dialog.conflictOverwrite": "Overwrite",

  // Listing failures; the keys match what the backend reports
  "panel.error.notFound": "Folder not found",
  "panel.error.accessDenied": "Access denied",
  "panel.error.unreachable": "Could not reach the network volume",
  "panel.error.credentials": "The volume refused these credentials",
  "panel.error.io": "Could not read the folder",
  "panel.error.unknown": "Could not load the folder",

  "statusbar.selection": "{count} of {total} selected",
  "statusbar.transfer": "{percent}% · {speed}",
  "statusbar.freeSpace": "{volume} · {free}",
  "statusbar.free": "{size} free",
  "statusbar.total": "{size} total",
  "statusbar.cancel": "Cancel transfer",
  "statusbar.cancelAll": "Cancel all transfers",
  "statusbar.transfersN": "{count} transfers",

  "transfer.queued": "Queued: {name}",
  "transfer.retrying": "Retrying {attempt}/{of}…",
  "transfer.nothingToTransfer": "Nothing selected to transfer",
  "transfer.zipRemoteOnly": "Download as Zip works on the remote panel",
  "transfer.intoItself": "Can't copy or move \"{name}\" into itself",

  // The transfer queue panel
  "queue.title": "Transfers",
  "queue.titleFor": "Transfers · {volume}",
  "queue.empty": "No transfers",
  "queue.clearFinished": "Clear finished",
  "queue.cancelAll": "Cancel all",
  "queue.remove": "Remove from list",
  "queue.status.queued": "Queued",
  "queue.status.done": "Done",
  "queue.status.error": "Failed",
  "queue.status.canceled": "Canceled",
  "queue.pending": "ready to paste",
  "queue.clipCopy": "Copy",
  "queue.clipMove": "Move",
  "queue.clipItems": "{count} items",
  "queue.clipClear": "Clear clipboard",

  "settings.title": "Settings",
  "settings.close": "Close settings",
  "settings.back": "Back to Main Window",
  "settings.nav.connections": "Connections",
  "settings.nav.appearance": "Appearance",
  "settings.nav.about": "About",

  "settings.appearance.heading": "Appearance",
  "settings.appearance.subline": "How the window looks and behaves.",
  "settings.osDrop.label": "Files dropped from outside",
  "settings.osDrop.subline":
    "When you drag files from your file manager onto a panel.",
  "settings.osDrop.copy": "Always copy",
  "settings.osDrop.menu": "Ask each time",

  // The file manager entry
  "settings.openHere.label": "Open in BG Bucket Browser",
  "settings.openHere.subline":
    "Adds this app to your file manager's \"Open With\" menu for folders. Doesn't change your default folder opener.",
  "settings.openHere.subline.mac":
    "Adds this app to Finder's right-click Services menu for folders. Doesn't change your default folder opener.",
  "settings.openHere.toggle": "Enabled",
  "settings.openHere.on": "Added to your file manager's \"Open With\" menu",
  "settings.openHere.on.mac": "Added to Finder's Services menu",
  "settings.openHere.off": "Removed from your file manager's \"Open With\" menu",
  "settings.openHere.off.mac": "Removed from Finder's Services menu",

  "settings.about.heading": "About",
  "settings.about.tagline":
    "Dual-pane file manager for Runpod network volumes (S3) and local disk.",
  "settings.about.version": "Version {version}",
  "settings.about.platform": "Linux · AppImage + deb",
  "settings.about.platform.windows": "Windows · MSI + NSIS",
  "settings.about.platform.mac": "macOS · DMG",
  "settings.about.createdBy": "Created by Bulent Gercek",
  "settings.about.contact":
    "For questions or just to get in touch, email me at {email}.",
  "settings.about.hope": "I hope you enjoy using BG Bucket Browser.",
  "settings.about.day": "Have a wonderful day!",

  "settings.connection.heading": "S3 endpoint",
  "settings.connection.subline":
    "Runpod network volumes speak the S3 API. Credentials are stored in the OS keychain.",

  "settings.connections.heading": "Connections",
  "settings.connections.add": "Add New Bucket Connection",
  "settings.connections.newName": "New connection",
  "settings.connections.rename": "Rename",
  "settings.connections.renameWithBucket": "Rename with bucket ID",
  "settings.connections.active": "active",
  "settings.connections.delete": "Delete connection",
  "settings.connections.discard": "Discard",
  "settings.connections.deleteTitle": "Delete connection?",
  "settings.connections.deleteBody":
    "Remove {name} and its stored keys from the OS keychain? This can't be undone. Files on the volume are not touched.",
  "settings.field.endpointUrl": "Endpoint URL",
  "settings.field.accessKeyId": "Access key ID",
  "settings.field.secretAccessKey": "Secret access key",
  "settings.field.bucketId": "Bucket / volume ID",
  "settings.field.region": "Region",
  "settings.field.keyStored": "•••• stored — leave blank to keep",
  "settings.connection.test": "Test connection",
  "settings.connection.testing": "Testing…",
  "settings.connection.reached": "Reached {bucket} in {ms} ms",

  "settings.maintenance.label": "Interrupted uploads",
  "settings.maintenance.subline":
    "Canceled or failed uploads leave partial data on the volume. Old ones (7+ days) are cleared automatically on launch.",
  "settings.maintenance.scan": "Scan for interrupted uploads",
  "settings.maintenance.scanning": "Scanning…",
  "settings.maintenance.none": "Nothing to clean up",
  "settings.maintenance.found": "{count} interrupted · {size} reclaimable",
  "settings.maintenance.cleanUp": "Clean up",
  "settings.maintenance.cleaning": "Cleaning up…",
  "settings.maintenance.done": "Cleaned up {count} upload(s)",
  "settings.maintenance.failed": "Could not reach the volume",

  // The RunPod account key, which is not an S3 key and not per connection
  "settings.runpod.label": "RunPod API key",
  "settings.runpod.subline":
    "Needed to show your RunPod volume's total size. Read-Only is enough.",
  "settings.runpod.save": "Save",
  "settings.runpod.saved": "RunPod API key saved",
  "settings.runpod.cleared": "RunPod API key removed",
  "dialog.cleanUpTitle": "Clean up interrupted uploads?",
  "dialog.cleanUpBody":
    "{count} unfinished upload(s) ({size}) will be aborted on the network volume. Any that you plan to resume will have to start over. This can't be undone.",

  "dialog.switchConnTitle": "Switch connection?",
  "dialog.switchConnBody":
    "The remote panel will switch to {name} (bucket {bucket}). Any transfers already running keep going on the current connection — nothing is interrupted.",
  "dialog.switchConnConfirm": "Switch",

  "main.volumeCleanup": "Volume Cleanup",
  "main.clipboard": "Clipboard",
  "cleanup.title": "Volume Cleanup — {volume}",
  "cleanup.intro":
    "Scans the active connection's bucket and reports where its space is going: folder breakdown, largest objects, and common reclaimable junk (__pycache__, .ipynb_checkpoints, 0-byte files). Nothing is deleted until you select items and confirm.",
  "cleanup.runScan": "Run Scan",
  "cleanup.scanning": "Scanning… {count} objects, {size}",
  "cleanup.cancel": "Cancel",
  "cleanup.summary": "{count} objects · {size} total",
  "cleanup.expandTop": "Expand Top Lists",
  "cleanup.expandSkipped": "Expand Not Scanned",
  "cleanup.expandReclaimable": "Expand Reclaimable",
  "cleanup.topFolders": "Top folders",
  "cleanup.largest": "Largest objects",
  "cleanup.reclaimable": "Reclaimable",
  "cleanup.reclaimableEmpty": "No obvious junk found.",
  "cleanup.skipped": "Not scanned",
  "cleanup.skippedHint": ".git, node_modules and site-packages folders are not counted.",
  "cleanup.selectAllReclaimable": "Select all reclaimable",
  "cleanup.clearSelection": "Clear selection",
  "cleanup.selectedSummary": "{count} selected · {size}",
  "cleanup.deleteSelected": "Delete selected",
  "cleanup.deleting": "Deleting…",
  "cleanup.deleted": "{count} object(s) deleted.",
  "cleanup.confirmTitle": "Delete {count} object(s)?",
  "cleanup.confirmBody":
    "This permanently deletes the selected objects from the bucket — S3 has no trash, this can't be undone.",
  "cleanup.confirmDelete": "Delete",
  "cleanup.rescan": "Scan again",
  "cleanup.abandonTitle": "Cancel the scan?",
  "cleanup.abandonBody":
    "The scan is still running. Closing now cancels it completely — you'll need to run it again to see results.",
  "cleanup.abandonConfirm": "Cancel scan",
  "cleanup.continueCleanup": "Continue Cleanup",

  "settings.theme.label": "Theme",
  "settings.theme.subline": "Applies to the whole window.",
  "settings.theme.dark": "Dark",
  "settings.theme.light": "Light",
  "settings.theme.system": "System",

  "settings.action.revert": "Revert",
  "settings.action.save": "Save",

  // Connection test failures; the keys match what the backend reports
  "testError.badSignature":
    "Authentication failed — the secret access key looks wrong",
  "testError.unknownAccessKey": "Access key ID not recognised",
  "testError.accessDenied":
    "Access denied — this key has no permission on that bucket",
  "testError.noSuchBucket": "Bucket not found — check the Bucket / volume ID",
  "testError.serviceError": "The server returned an error",
  "testError.unreachable":
    "Could not reach the server — check the endpoint address and your network",
  "testError.timeout": "Timed out — the server did not respond in time",
  "testError.badResponse": "The server sent an unexpected response",
  "testError.badRequest":
    "Could not build the request — the endpoint URL may be malformed",
  "testError.noCredentials":
    "No credentials — enter the access and secret key, or Save first",
  "testError.unknown": "Connection failed",
} as const;

export type StringKey = keyof typeof en;

/** Looks up a string and fills in its placeholders. */
export function t(key: StringKey, vars?: Record<string, string | number>): string {
  let s: string = en[key];
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      s = s.replaceAll(`{${k}}`, String(v));
    }
  }
  return s;
}
