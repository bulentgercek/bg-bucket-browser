//! Connections: what the app knows about each network volume, and where the
//! credentials live.
//!
//! The non-secret fields (name, endpoint, region, bucket) sit in the app's
//! store file; the access and secret keys only ever live in the operating
//! system's keychain, one account per connection. Nothing here hands a key
//! value back to the frontend — only whether one is stored.
//!
//! One connection is active at a time, so the listing and file commands take no
//! connection argument. Transfers are the exception: they remember the
//! connection they started on.

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use aws_sdk_s3::Client;

use crate::core::{BoxErr, s3_client_from_parts};

const STORE_FILE: &str = "app-state.json";
const K_CONNS: &str = "connections";
const K_ACTIVE: &str = "activeConnId";
// The single-connection shape this app started with; read once, during migration.
const K_OLD_CONN: &str = "connection";
const KEYRING_SERVICE: &str = "bg-bucket-browser";
const KR_OLD_ACCESS: &str = "s3-access-key";
const KR_OLD_SECRET: &str = "s3-secret-key";

fn kr_acc(id: &str) -> String {
    format!("conn/{id}/access")
}
fn kr_sec(id: &str) -> String {
    format!("conn/{id}/secret")
}

/// A connection as stored on disk: everything that is not a secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnEntry {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
}

/// A connection as the frontend sees it: the same fields plus whether each key
/// is present, never the key values themselves.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnInfo {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub has_access_key: bool,
    pub has_secret_key: bool,
}

/// The old single-connection shape, parsed only while migrating.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OldConnConfig {
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    bucket: String,
}

// ── Keychain ─────────────────────────────────────────────
// A missing or empty entry is simply "no key": every caller treats an absent
// credential as a connection that is not ready yet.
pub(crate) fn kr_get(account: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, account)
        .ok()?
        .get_password()
        .ok()
        .filter(|s| !s.is_empty())
}

pub(crate) fn kr_set(account: &str, value: &str) -> Result<(), BoxErr> {
    keyring::Entry::new(KEYRING_SERVICE, account)?
        .set_password(value)
        .map_err(Into::into)
}

pub(crate) fn kr_del(account: &str) {
    if let Ok(e) = keyring::Entry::new(KEYRING_SERVICE, account) {
        let _ = e.delete_credential();
    }
}

// ── The RunPod account API key ──
// Separate from the S3 keys and not tied to a connection: it belongs to the
// RunPod account, and every volume in that account is read with the same key.
const KR_RUNPOD_KEY: &str = "runpod-api-key";

/// Stores the key, or clears it when the field is left empty.
#[tauri::command]
pub fn set_runpod_api_key(key: String) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        kr_del(KR_RUNPOD_KEY);
    } else {
        kr_set(KR_RUNPOD_KEY, key).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// A keychain read takes tens of milliseconds, and a locked keychain may wait
// for the user; `async` keeps that wait off the main thread.
#[tauri::command(async)]
pub fn has_runpod_api_key() -> bool {
    kr_get(KR_RUNPOD_KEY).is_some()
}

pub fn runpod_api_key() -> Option<String> {
    kr_get(KR_RUNPOD_KEY)
}

// ── Reading and writing the store ────────────────────────────────────────
// A store that cannot be opened or parsed reads as "no connections yet", which
// is the same state as a fresh install.
fn read_conns(app: &AppHandle) -> Vec<ConnEntry> {
    let Ok(store) = app.store(STORE_FILE) else {
        return Vec::new();
    };
    store
        .get(K_CONNS)
        .and_then(|v| serde_json::from_value::<Vec<ConnEntry>>(v).ok())
        .unwrap_or_default()
}

fn read_active(app: &AppHandle) -> Option<String> {
    let store = app.store(STORE_FILE).ok()?;
    store
        .get(K_ACTIVE)?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn write_conns(app: &AppHandle, conns: &[ConnEntry]) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set(
        K_CONNS,
        serde_json::to_value(conns).map_err(|e| e.to_string())?,
    );
    store.save().map_err(|e| e.to_string())
}

fn write_active(app: &AppHandle, id: &str) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    store.set(K_ACTIVE, serde_json::Value::String(id.to_string()));
    store.save().map_err(|e| e.to_string())
}

// Falls back to the first connection when no active one was ever recorded.
fn active_id_inner(app: &AppHandle) -> Option<String> {
    read_active(app).or_else(|| read_conns(app).first().map(|c| c.id.clone()))
}

// ── Migration ───────────────────────────────────────────────────────
/// Moves a pre-multi-connection setup into the connection list, once.
///
/// Idempotent and called before every config read, so an old install upgrades
/// on whatever happens to run first.
fn migrate_if_needed(app: &AppHandle) {
    if !read_conns(app).is_empty() {
        return;
    }
    let Ok(store) = app.store(STORE_FILE) else {
        return;
    };
    let Some(old) = store
        .get(K_OLD_CONN)
        .and_then(|v| serde_json::from_value::<OldConnConfig>(v).ok())
    else {
        return;
    };
    if old.endpoint.is_empty() && old.bucket.is_empty() {
        return;
    }
    let id = "conn-1".to_string();
    let name = if old.bucket.is_empty() {
        "Connection 1".to_string()
    } else {
        old.bucket.clone()
    };
    if let Some(ak) = kr_get(KR_OLD_ACCESS) {
        let _ = kr_set(&kr_acc(&id), &ak);
    }
    if let Some(sk) = kr_get(KR_OLD_SECRET) {
        let _ = kr_set(&kr_sec(&id), &sk);
    }
    let entry = ConnEntry {
        id: id.clone(),
        name,
        endpoint: old.endpoint,
        region: old.region,
        bucket: old.bucket,
    };
    let _ = write_conns(app, &[entry]);
    let _ = write_active(app, &id);
}

// Reading the keychain here is what turns a stored entry into "has keys".
fn to_info(c: &ConnEntry) -> ConnInfo {
    ConnInfo {
        id: c.id.clone(),
        name: c.name.clone(),
        endpoint: c.endpoint.clone(),
        region: c.region.clone(),
        bucket: c.bucket.clone(),
        has_access_key: kr_get(&kr_acc(&c.id)).is_some(),
        has_secret_key: kr_get(&kr_sec(&c.id)).is_some(),
    }
}

// ── Commands ─────────────────────────────────────────────────────

// Reads the keychain for every connection; off the main thread, see `has_runpod_api_key`.
#[tauri::command(async)]
pub fn list_connections(app: AppHandle) -> Vec<ConnInfo> {
    migrate_if_needed(&app);
    read_conns(&app)
        .iter()
        .map(to_info)
        .collect()
}

#[tauri::command]
pub fn active_connection_id(app: AppHandle) -> Option<String> {
    migrate_if_needed(&app);
    active_id_inner(&app)
}

/// Switches the active connection. Running transfers are unaffected: each one
/// keeps working on the connection it was queued with.
#[tauri::command]
pub fn set_active_connection(app: AppHandle, id: String) -> Result<(), String> {
    if !read_conns(&app).iter().any(|c| c.id == id) {
        return Err("notFound".into());
    }
    write_active(&app, &id)
}

#[tauri::command]
pub fn rename_connection(
    app: AppHandle,
    id: String,
    name: String,
) -> Result<(), String> {
    let mut conns = read_conns(&app);
    let c = conns.iter_mut().find(|c| c.id == id).ok_or("notFound")?;
    c.name = name;
    write_conns(&app, &conns)
}

/// Deletes a connection along with its stored keys.
///
/// The last remaining connection and the first one cannot be deleted, so the
/// app always has a connection slot to show in Settings.
#[tauri::command]
pub fn delete_connection(app: AppHandle, id: String) -> Result<(), String> {
    let mut conns = read_conns(&app);
    if conns.len() <= 1 {
        return Err("lastConnection".into());
    }
    if conns.first().map(|c| c.id == id).unwrap_or(false) {
        return Err("firstConnection".into());
    }
    let existed = conns.iter().any(|c| c.id == id);
    if !existed {
        return Err("notFound".into());
    }
    conns.retain(|c| c.id != id);
    write_conns(&app, &conns)?;
    forget_client(&id);
    kr_del(&kr_acc(&id));
    kr_del(&kr_sec(&id));
    if read_active(&app).as_deref() == Some(id.as_str()) {
        let first = conns[0].id.clone();
        write_active(&app, &first)?;
    }
    Ok(())
}

/// Adds or updates one connection and returns its id.
///
/// An empty `access_key` or `secret_key` leaves the stored key untouched, which
/// is how the UI can show a saved connection without ever holding its secrets.
/// A missing `id` means the active connection, so a first-run save creates one.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn save_connection(
    app: AppHandle,
    id: Option<String>,
    name: Option<String>,
    endpoint: String,
    region: String,
    bucket: String,
    access_key: Option<String>,
    secret_key: Option<String>,
) -> Result<String, String> {
    migrate_if_needed(&app);
    let mut conns = read_conns(&app);

    let cid = id
        .filter(|s| !s.is_empty())
        .or_else(|| active_id_inner(&app))
        .unwrap_or_else(|| "conn-1".to_string());

    let existing = conns.iter().find(|c| c.id == cid).cloned();
    // An unnamed connection falls back to its bucket name, which is what the
    // sidebar would otherwise show as blank.
    let final_name = name
        .filter(|s| !s.trim().is_empty())
        .or_else(|| existing.as_ref().map(|c| c.name.clone()))
        .unwrap_or_else(|| {
            if bucket.is_empty() {
                "Connection".to_string()
            } else {
                bucket.clone()
            }
        });

    let entry = ConnEntry {
        id: cid.clone(),
        name: final_name,
        endpoint,
        region,
        bucket,
    };
    match conns.iter_mut().find(|c| c.id == cid) {
        Some(c) => *c = entry,
        None => conns.push(entry),
    }
    write_conns(&app, &conns)?;
    // Dropped before and after the keys are written; a command running in between may cache the old keys.
    forget_client(&cid);
    if read_active(&app).is_none() {
        write_active(&app, &cid)?;
    }

    if let Some(ak) = access_key.filter(|s| !s.is_empty()) {
        kr_set(&kr_acc(&cid), &ak).map_err(|e| e.to_string())?;
    }
    if let Some(sk) = secret_key.filter(|s| !s.is_empty()) {
        kr_set(&kr_sec(&cid), &sk).map_err(|e| e.to_string())?;
    }
    forget_client(&cid);
    Ok(cid)
}

/// The active connection's info, for the older frontend entry point.
///
/// Today's Settings screen reads the whole list instead.
#[tauri::command(async)]
pub fn load_connection(app: AppHandle) -> ConnInfo {
    migrate_if_needed(&app);
    let conns = read_conns(&app);
    let active = active_id_inner(&app).unwrap_or_default();
    match conns.iter().find(|c| c.id == active).or_else(|| conns.first()) {
        Some(c) => to_info(c),
        None => ConnInfo {
            id: String::new(),
            name: String::new(),
            endpoint: String::new(),
            region: String::new(),
            bucket: String::new(),
            has_access_key: false,
            has_secret_key: false,
        },
    }
}

/// The stored keys a connection test should fall back to when the user left the
/// key fields blank.
pub fn stored_keys_for(
    app: &AppHandle,
    id: Option<&str>,
) -> (Option<String>, Option<String>) {
    let cid = id
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| active_id_inner(app));
    match cid {
        Some(i) => (kr_get(&kr_acc(&i)), kr_get(&kr_sec(&i))),
        None => (kr_get(KR_OLD_ACCESS), kr_get(KR_OLD_SECRET)),
    }
}

// ── S3 client cache ──────────────────────────────────────────────────
/// One client per connection id. A client keeps its connection pool, so reusing
/// it skips the keychain lookup and the TCP/TLS handshake on every command.
static CLIENTS: LazyLock<Mutex<HashMap<String, (Client, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// A poisoned lock is recovered rather than propagated: the map holds clients,
// and a half-written entry is not a correctness problem here.
fn clients() -> std::sync::MutexGuard<'static, HashMap<String, (Client, String)>> {
    CLIENTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drops the cached client so the next call reads the connection again.
fn forget_client(id: &str) {
    clients().remove(id);
}

fn client_for_entry(c: &ConnEntry) -> Result<(Client, String), BoxErr> {
    if let Some(hit) = clients().get(&c.id) {
        return Ok(hit.clone());
    }
    let (ak, sk) = kr_get(&kr_acc(&c.id))
        .zip(kr_get(&kr_sec(&c.id)))
        .ok_or_else(|| format!("credentials missing for connection {}", c.id))?;
    let pair = s3_client_from_parts(&c.endpoint, &c.region, &c.bucket, &ak, &sk);
    clients().insert(c.id.clone(), pair.clone());
    crate::devlog::verbose("s3", format!("client built for {}", c.id));
    Ok(pair)
}

/// S3 client and bucket name of the active connection. Fails if it is missing
/// endpoint, region, bucket or credentials; there is no other source of credentials.
pub fn active_client(app: &AppHandle) -> Result<(Client, String), BoxErr> {
    migrate_if_needed(app);
    let conns = read_conns(app);
    let active = active_id_inner(app);
    let c = active
        .as_deref()
        .and_then(|id| conns.iter().find(|c| c.id == id))
        .or(conns.first())
        .filter(|c| !c.endpoint.is_empty() && !c.region.is_empty() && !c.bucket.is_empty())
        .ok_or("active connection is incomplete (missing endpoint, region, bucket or credentials)")?;
    client_for_entry(c).map_err(|_| {
        "active connection is incomplete (missing endpoint, region, bucket or credentials)".into()
    })
}

/// S3 client and bucket name of the connection with this id, active or not.
pub fn client_by_id(app: &AppHandle, id: &str) -> Result<(Client, String), BoxErr> {
    let conns = read_conns(app);
    let c = conns
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| format!("connection {id} not found"))?;
    client_for_entry(c)
}
