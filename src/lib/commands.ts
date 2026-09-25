import { invoke as rawInvoke, type InvokeArgs } from "@tauri-apps/api/core";

/* Typed counterparts of the backend commands, and the only place that calls
   them: no component reaches for a raw invoke.

   The types here are written by hand and have to be kept in step with the Rust
   side; a generator was left for later. */

/* Every call below goes through this wrapper, so the verbose development log
   records what was requested and how it ended (`devlog.rs`). */

/** Commands whose arguments are never logged (they carry credentials). */
const LOG_NO_ARGS = new Set([
  "test_connection",
  "save_connection",
  "set_runpod_api_key",
  // The user's own words and address stay out of the log.
  "feedback_send",
]);

/** High-frequency commands: logged only when they fail. */
const LOG_ERRORS_ONLY = new Set([
  "read_os_clipboard_files",
  "get_thumbnail",
  "disk_usage",
  "list_devices",
  "volume_quota",
]);

const LOG_MAX_CHARS = 600;

function clip(text: string): string {
  return text.length > LOG_MAX_CHARS
    ? `${text.slice(0, LOG_MAX_CHARS)}…(+${text.length - LOG_MAX_CHARS} chars)`
    : text;
}

/** Credential-like field names (`accessKey`, `api_key`, `secret`…). A bare `key` is an
    S3 object path here and stays visible; `set_runpod_api_key` is in LOG_NO_ARGS. */
function isSecretField(name: string): boolean {
  return /secret|token|password/i.test(name) || /[a-z]Key$|_key$/.test(name);
}

/** JSON with credential-like fields masked. */
function maskedJson(value: unknown): string {
  try {
    return clip(JSON.stringify(value, (k, v) => (isSecretField(k) ? "***" : v)) ?? String(value));
  } catch {
    return String(value);
  }
}

async function invoke<T = void>(cmd: string, args?: InvokeArgs): Promise<T> {
  const quiet = LOG_ERRORS_ONLY.has(cmd);
  const started = performance.now();
  if (!quiet) {
    const shown = args === undefined ? "" : ` ${LOG_NO_ARGS.has(cmd) ? "{…}" : maskedJson(args)}`;
    devlogVerbose("cmd", `→ ${cmd}${shown}`);
  }
  try {
    const result = await rawInvoke<T>(cmd, args);
    if (!quiet) devlogVerbose("cmd", `← ${cmd} ok ${Math.round(performance.now() - started)}ms`);
    return result;
  } catch (err) {
    devlogVerbose(
      "cmd",
      `← ${cmd} error ${Math.round(performance.now() - started)}ms ${maskedJson(err)}`,
    );
    throw err;
  }
}

export interface TestConnectionArgs {
  /** Whose stored keys to fall back on when the key fields are left blank; the
      live connection when this is not given, and nothing for a connection that
      has not been saved yet. */
  id?: string;
  endpoint: string;
  region: string;
  bucket: string;
  /** Left out to test with the stored keys. */
  accessKey?: string;
  secretKey?: string;
}

export interface TestOk {
  /** How long connecting and listing took. */
  ms: number;
  /** The bucket that answered. */
  bucket: string;
}

/** Why a connection test failed; the sentence the user reads is chosen from
    this. */
export type TestErrKind =
  | "badSignature"
  | "unknownAccessKey"
  | "accessDenied"
  | "noSuchBucket"
  | "serviceError"
  | "unreachable"
  | "timeout"
  | "badResponse"
  | "badRequest"
  | "noCredentials"
  | "unknown";

/** A failed connection test, as the rejection value. */
export interface TestErr {
  /** Which failure this is. */
  kind: TestErrKind;
  /** The SDK's own short message, for the log and for support. */
  detail: string;
  /** The S3 error code, when the service sent one. */
  code: string | null;
  /** The HTTP status, when a response arrived at all. */
  httpStatus: number | null;
}

/**
 * Tests a connection by really connecting and listing, rather than by checking
 * the fields. Rejects with a `TestErr`.
 */
export function testConnection(args: TestConnectionArgs): Promise<TestOk> {
  return invoke<TestOk>("test_connection", { ...args });
}

/* ── Creating, renaming and deleting ─────────────────────── */

export type FsErrKind =
  | "notFound"
  | "accessDenied"
  | "alreadyExists"
  | "unsupported"
  | "io"
  | "s3"
  | "unknown";

export interface FsErr {
  kind: FsErrKind;
  detail: string;
}

export interface DelItem {
  /** The item's full path, as its side writes it. */
  path: string;
  isDir: boolean;
}

/** Creates a folder and returns the name it actually got: a remote name may
 *  come back cleaned, and the caller is expected to compare and say so. */
export function createFolder(
  remote: boolean,
  parent: string,
  name: string,
): Promise<string> {
  return invoke<string>("create_folder", { remote, parent, name });
}

/** Creates an empty file; returns the name as `createFolder` does. */
export function createFile(
  remote: boolean,
  parent: string,
  name: string,
): Promise<string> {
  return invoke<string>("create_file", { remote, parent, name });
}

/** Renames one item in place; `newName` is a name, not a path. */
export function renameEntry(
  remote: boolean,
  path: string,
  isDir: boolean,
  newName: string,
): Promise<string> {
  return invoke<string>("rename", { remote, path, isDir, newName });
}

/** Deletes a selection. Locally this can go to the trash; remote deletion is
    always permanent. */
export function deleteEntries(
  remote: boolean,
  permanent: boolean,
  items: DelItem[],
): Promise<void> {
  return invoke("delete", { remote, permanent, items });
}

export interface FolderSize {
  bytes: number;
  files: number;
}

/** The recursive size of a folder. Slow on a large remote folder: it is one
    request per page of objects. */
export function folderSize(
  remote: boolean,
  path: string,
): Promise<FolderSize> {
  return invoke<FolderSize>("folder_size", { remote, path });
}

/* ── The transfer queue ─────────────────────── */

export type TransferKind =
  | "download"
  | "upload"
  | "downloadZip"
  | "openDownload" // download to the cache, then open it
  | "localCopy" // both ends local
  | "localMove"
  | "remoteCopy" // both ends remote
  | "remoteMove";

/** Queues a transfer and returns its id; progress arrives as events. */
export type ConflictPolicy = "skip" | "overwrite" | "rename";

export function transferStart(
  kind: TransferKind,
  src: string,
  destDir: string,
  name: string,
  isDir = false,
  moveSrc = false,
  /** What to do about a name that already exists at the destination. The
      backend picks the free name itself when renaming; the name passed here is
      only a first guess. */
  conflict: ConflictPolicy = "skip",
): Promise<string> {
  return invoke<string>("transfer_start", {
    kind,
    src,
    destDir,
    name,
    isDir,
    moveSrc,
    conflict,
  });
}

/**
 * Opens a remote file: it is downloaded to a cache through the queue and then
 * handed to the operating system. The copy is a copy — editing it changes
 * nothing on the volume. There is no size limit; a large file can be cancelled
 * from the queue like any other transfer.
 */
export function openRemoteStart(path: string, name: string): Promise<string> {
  return invoke<string>("open_remote_start", { path, name });
}

/** Streams the selected remote items into one archive on the local disk. */
export function transferZipStart(
  srcs: { path: string; isDir: boolean }[],
  destDir: string,
  name: string,
): Promise<string> {
  return invoke<string>("transfer_zip_start", { srcs, destDir, name });
}

/** Cancels one transfer; a partial download stays on disk to resume from. */
export function transferCancel(id: string): Promise<void> {
  return invoke("transfer_cancel", { id });
}

/** Cancels everything, running and waiting. */
export function transferCancelAll(): Promise<void> {
  return invoke("transfer_cancel_all");
}

/* ── Unfinished uploads ─────────────────────────────
   A multipart upload that was never completed or aborted keeps its parts on the
   volume. Old ones are swept at startup; this is the manual cleanup in
   Settings. */

export interface OrphanUpload {
  key: string;
  uploadId: string;
  /** When it started, when the service says. */
  initiatedMs: number | null;
  /** How much of it is already on the volume. */
  bytes: number;
}

/** Every multipart upload still open on the volume. */
export function listOrphanUploads(): Promise<OrphanUpload[]> {
  return invoke<OrphanUpload[]>("list_orphan_uploads");
}

/** Aborts the given uploads and returns how many really went. */
export function abortOrphanUploads(
  uploads: { key: string; uploadId: string }[],
): Promise<number> {
  return invoke<number>("abort_orphan_uploads", { uploads });
}

/* ── Local disks ──────────────────────────── */

export interface Device {
  label: string;
  /** Where it is mounted; the pane opens at this path. */
  mountPath: string;
  kind: "ssd" | "hdd" | "removable" | "disk";
  total: number;
  free: number;
}

/** The disks worth showing, with system and pseudo mounts left out. */
export function listDevices(): Promise<Device[]> {
  return invoke<Device[]>("list_devices");
}

export interface DiskUsage {
  label: string;
  free: number;
  total: number;
}

/** Free and total space of whatever filesystem a path is on. */
export function diskUsage(path: string): Promise<DiskUsage> {
  return invoke<DiskUsage>("disk_usage", { path });
}

/** The home directory's real path, for display only: paths are stored and
    navigated as `~`. */
export function localHomeDir(): Promise<string> {
  return invoke<string>("local_home_dir");
}

/* ── Connections ────── */

/** A connection as the frontend may know it: every field except the secrets,
    plus whether each key is stored. */
export interface ConnectionInfo {
  id: string;
  name: string;
  endpoint: string;
  region: string;
  bucket: string;
  hasAccessKey: boolean;
  hasSecretKey: boolean;
}

/** Mirrors `resolve_connection` (config.rs): usable only with every field and both keys. */
export function isConnectionComplete(c: ConnectionInfo): boolean {
  return c.hasAccessKey && c.hasSecretKey && !!c.endpoint && !!c.region && !!c.bucket;
}

/** Every configured connection. */
export function listConnections(): Promise<ConnectionInfo[]> {
  return invoke<ConnectionInfo[]>("list_connections");
}

/** The live connection's id. */
export function activeConnectionId(): Promise<string | null> {
  return invoke<string | null>("active_connection_id");
}

/** Switches the live connection; running transfers keep their own. */
export function setActiveConnection(id: string): Promise<void> {
  return invoke("set_active_connection", { id });
}

export function renameConnection(id: string, name: string): Promise<void> {
  return invoke("rename_connection", { id, name });
}

/** Deletes a connection, with the keys it owns. */
export function deleteConnection(id: string): Promise<void> {
  return invoke("delete_connection", { id });
}

/** The live connection alone; the list is what today's screens read. */
export function loadConnection(): Promise<ConnectionInfo> {
  return invoke<ConnectionInfo>("load_connection");
}

/** The RunPod account key, which is not an S3 key and not per connection.
 *  An empty string removes it. */
export function setRunpodApiKey(key: string): Promise<void> {
  return invoke("set_runpod_api_key", { key });
}
export function hasRunpodApiKey(): Promise<boolean> {
  return invoke<boolean>("has_runpod_api_key");
}

/** The volume's total capacity, which S3 itself has no concept of: it comes
 *  from the RunPod account API and needs that key.
 *
 *  Total only. Used and free space is not available anywhere without a running
 *  pod, and this app exists to avoid starting one. */
export interface VolumeQuota {
  totalBytes: number;
}
export function volumeQuota(): Promise<VolumeQuota> {
  return invoke<VolumeQuota>("volume_quota");
}

/** A feedback recording, as the Feedback screen shows it (`feedback.rs`). */
export interface RecordingInfo {
  text: string;
  /** Log lines, not counting the start and end markers. */
  lines: number;
  durationSec: number;
  /** Still capturing. */
  active: boolean;
  /** The app went down while recording. */
  cutShort: boolean;
}

/** Why a report was not accepted. */
export interface FeedbackErr {
  kind: "invalid" | "tooLarge" | "rateLimited" | "server" | "network";
  /** For `invalid`: the field the service rejected, when it names one. */
  field?: string;
}

export function feedbackRecordStart(): Promise<void> {
  return invoke("feedback_record_start");
}
export function feedbackRecordStop(): Promise<RecordingInfo | null> {
  return invoke<RecordingInfo | null>("feedback_record_stop");
}
/** The recording waiting to be sent: running, finished or cut short. */
export function feedbackRecording(): Promise<RecordingInfo | null> {
  return invoke<RecordingInfo | null>("feedback_recording");
}
export function feedbackDiscardRecording(): Promise<void> {
  return invoke("feedback_discard_recording");
}
/** Whether reports go to the service's test channel (development builds). */
export function feedbackIsTest(): Promise<boolean> {
  return invoke<boolean>("feedback_is_test");
}
/** Sends a report; resolves to the service's reference for it. */
export function feedbackSend(
  message: string,
  contact: string,
  withRecording: boolean,
): Promise<string> {
  return invoke<string>("feedback_send", { message, contact, withRecording });
}

/** Whether the file manager entry is registered. Each platform registers it
 *  differently; all three are per-user and change no system default. */
export function isOpenHereRegistered(): Promise<boolean> {
  return invoke<boolean>("is_open_here_registered");
}
export function registerOpenHere(enable: boolean): Promise<void> {
  return invoke("register_open_here", { enable });
}
/** The folder this launch was asked to open, asked for once at startup. */
export function takeStartupPath(): Promise<string | null> {
  return invoke<string | null>("take_startup_path");
}

export interface SaveConnectionArgs {
  /** Which connection to write; without it, the live one, or a new one. */
  id?: string;
  name?: string;
  endpoint: string;
  region: string;
  bucket: string;
  /** Left out to keep the stored key as it is. */
  accessKey?: string;
  secretKey?: string;
}

/** Adds or updates a connection and returns its id. */
export function saveConnection(args: SaveConnectionArgs): Promise<string> {
  return invoke<string>("save_connection", { ...args });
}

/* ── Listing ─────────────── */

/** One entry of a directory, the same shape for both sides. */
export interface DirEntry {
  /** The entry's own name, never a path. */
  name: string;
  kind: "dir" | "file";
  /** Size in bytes; a directory has none. */
  size: number | null;
  /** Modification time in epoch milliseconds. */
  modified: number | null;
  /** Icon hint; a plain file has none. */
  glyph: "text" | "zip" | "video" | null;
}

/** Why a listing failed. */
export type ListErrKind =
  | "notFound"
  | "accessDenied"
  | "unreachable"
  | "credentials"
  | "io"
  | "unknown";

/** A failed listing, as the rejection value. */
export interface ListErr {
  /** Which failure this is. */
  kind: ListErrKind;
  /** The underlying short message, for the log. */
  detail: string;
}

/**
 * Lists a remote directory, given a path relative to the volume root. Entries
 * arrive sorted, directories first. Rejects with a `ListErr`.
 */
export function listRemote(path: string): Promise<DirEntry[]> {
  return invoke<DirEntry[]>("list_remote", { path });
}

/**
 * Lists a local directory. `~` means the home directory; anything else is an
 * absolute path. Same contract as `listRemote`.
 */
export function listLocal(path: string): Promise<DirEntry[]> {
  return invoke<DirEntry[]>("list_local", { path });
}

/** What absolute paths dropped onto the window turn out to be; unreadable
    ones are left out, so the result can be shorter than the input. */
export interface PathStat {
  path: string;
  name: string;
  isDir: boolean;
  size: number | null;
  modified: number | null;
}
export function statPaths(paths: string[]): Promise<PathStat[]> {
  return invoke<PathStat[]>("stat_paths", { paths });
}

/* ── Thumbnails ─────────────────────── */

/**
 * Asks for one thumbnail, for either side and for images as well as videos.
 *
 * A data URI can go straight into an image tag. `null` means there is
 * legitimately no preview and the tile should keep its type icon; a rejection
 * is a real failure, which the tile survives the same way.
 */
export function getThumbnail(
  side: "local" | "remote",
  path: string,
): Promise<string | null> {
  return invoke<string | null>("get_thumbnail", { side, path });
}

/* ── Opening things in the operating system ──────────────── */

/**
 * Opens a local file in its default application, or a folder in the system file
 * manager. A remote path is rejected here on purpose: opening one goes through
 * `openRemoteStart`, which downloads it first.
 */
export function openPath(
  side: "local" | "remote",
  path: string,
): Promise<void> {
  return invoke("open_path", { side, path });
}

/** Opens a link in the browser or mail client; other schemes are refused. */
export function openUrl(url: string): Promise<void> {
  return invoke("open_url", { url });
}

/* ── Volume Cleanup ───────────────────────
   Scanning a large volume takes a while; cancelling it resolves the scan with
   `null`, which is an answer rather than a failure. */

export interface ScanEntry {
  key: string;
  size: number;
}

export interface FolderStat {
  prefix: string;
  isFile: boolean;
  bytes: number;
  count: number;
}

export interface ScanReport {
  totalObjects: number;
  totalBytes: number;
  topFolders: FolderStat[];
  largest: ScanEntry[];
  reclaimable: ScanEntry[];
  /** Folders the scan did not enter (`.git`, `node_modules`, `site-packages`); not in the totals. */
  skipped: string[];
}

/** How far the scan has got, sent periodically while it runs. */
export interface ScanProgress {
  scanned: number;
  bytes: number;
}

export function scanVolume(): Promise<ScanReport | null> {
  return invoke<ScanReport | null>("scan_volume");
}

export function cancelScan(): Promise<void> {
  return invoke("cancel_scan");
}

/** Deletes the given objects and returns how many really went. */
export function deleteScanned(keys: string[]): Promise<number> {
  return invoke<number>("delete_scanned", { keys });
}

/** What the OS clipboard holds, when it holds files. An empty list is not a
 *  failure: it means there is nothing to paste. A cut is only detectable on
 *  some desktops, and anything else reads as a copy. */
export interface OsClipboardState {
  paths: string[];
  cut: boolean;
}
export function readOsClipboardFiles(): Promise<OsClipboardState> {
  return invoke<OsClipboardState>("read_os_clipboard_files");
}

/** Writes local paths to the OS clipboard so other applications can paste
 *  them. Returns whether a requested cut could really be marked as one. */
export function writeOsClipboardFiles(
  paths: string[],
  cut: boolean,
): Promise<boolean> {
  return invoke<boolean>("write_os_clipboard_files", { paths, cut });
}

/** Starts a real drag out of the window, for local paths.
 *
 *  A prototype: nothing in the list starts a drag with it yet, and it is
 *  reached only from a temporary context menu row. */
export function startOsDrag(paths: string[]): Promise<void> {
  return invoke("start_os_drag", { paths });
}

/** Development log, toast branch (`devlog.rs`). Fire-and-forget: logging never breaks the UI. */
export function devlogToast(tone: string, text: string): void {
  rawInvoke("devlog_toast", { tone, text }).catch(() => {});
}

/** Development log, verbose branch (`devlog.rs`). No-op in Rust when verbose is off. */
export function devlogVerbose(area: string, msg: string): void {
  rawInvoke("devlog_verbose", { area, msg }).catch(() => {});
}
