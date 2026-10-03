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
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Deletes one keychain entry; an entry that is already gone is not an error.
/// The entry is read back afterwards, because a backend may report a delete it
/// did not do.
pub(crate) fn kr_del(account: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, account).map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => return Err(e.to_string()),
    }
    match entry.get_password() {
        Err(keyring::Error::NoEntry) => Ok(()),
        Ok(_) => Err("the key is still in the keychain".into()),
        Err(e) => Err(e.to_string()),
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
        kr_del(KR_RUNPOD_KEY)?;
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
/// The old setup is settled once in a run: a keychain that refuses is not asked
/// again before every command.
static LEGACY_TRIED: AtomicBool = AtomicBool::new(false);

/// Moves a pre-multi-connection setup into the connection list, and clears
/// what that setup left behind: its two keychain entries and its record in the
/// store.
///
/// Called before every config read, so an old install upgrades on whatever
/// happens to run first. An install without the old record leaves here after
/// one read of the store.
fn migrate_if_needed(app: &AppHandle) {
    let Ok(store) = app.store(STORE_FILE) else {
        return;
    };
    let Some(old) = store.get(K_OLD_CONN) else {
        return;
    };
    if LEGACY_TRIED.swap(true, Ordering::Relaxed) {
        return;
    }
    let id = "conn-1";
    let mut conns = read_conns(app);
    if conns.is_empty()
        && let Ok(old) = serde_json::from_value::<OldConnConfig>(old)
        && !(old.endpoint.is_empty() && old.bucket.is_empty())
    {
        let name = if old.bucket.is_empty() {
            "Connection 1".to_string()
        } else {
            old.bucket.clone()
        };
        conns.push(ConnEntry {
            id: id.to_string(),
            name,
            endpoint: old.endpoint,
            region: old.region,
            bucket: old.bucket,
        });
        // Without the entry nothing else is touched: the old record and its
        // keys are still what the next start upgrades from.
        if write_conns(app, &conns).is_err() {
            return;
        }
        let _ = write_active(app, id);
    }
    let target = conns.iter().any(|c| c.id == id).then_some(id);
    match settle_legacy(&mut OsKeychain, target) {
        Ok(()) => {
            store.delete(K_OLD_CONN);
            let _ = store.save();
            crate::devlog::verbose("config", "the old single-connection setup was cleared");
        }
        // The old record stays, so the next start tries again.
        Err(e) => crate::devlog::verbose("config", format!("the old setup's keys were kept: {e}")),
    }
}

/// What becomes of the keys the single-connection setup stored, given the
/// connection that took its place: each goes to that connection unless it
/// holds a key of its own already, and then leaves the old place. A key that
/// could not be carried over is not removed.
fn settle_legacy(keys: &mut impl KeyStore, target: Option<&str>) -> Result<(), String> {
    let moves = [
        (KR_OLD_SECRET, target.map(kr_sec)),
        (KR_OLD_ACCESS, target.map(kr_acc)),
    ];
    for (old, new) in moves {
        let Some(value) = keys.get(old) else { continue };
        if let Some(new) = &new
            && keys.get(new).is_none()
        {
            keys.set(new, &value)?;
            if keys.get(new).as_deref() != Some(value.as_str()) {
                return Err("a key did not arrive in its new place".into());
            }
        }
        keys.del(old)?;
    }
    Ok(())
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
    // The keys go first, the secret before the access key: a connection is not
    // removed while its keys cannot be, and a half-done removal leaves the less
    // sensitive half behind.
    forget_client(&id);
    kr_del(&kr_sec(&id))?;
    kr_del(&kr_acc(&id))?;
    conns.retain(|c| c.id != id);
    write_conns(&app, &conns)?;
    forget_client(&id);
    if read_active(&app).as_deref() == Some(id.as_str()) {
        let first = conns[0].id.clone();
        write_active(&app, &first)?;
    }
    Ok(())
}

/// Removes a connection's two keys from the keychain and keeps the connection.
///
/// The first and the last connection cannot be deleted, and an empty key field
/// means "keep it": without this, their keys could only be removed with the
/// operating system's own keychain tool.
#[tauri::command]
pub fn remove_connection_keys(app: AppHandle, id: String) -> Result<(), String> {
    if !read_conns(&app).iter().any(|c| c.id == id) {
        return Err("notFound".into());
    }
    if crate::transfers::connection_busy(&app, &id) {
        return Err("busy".into());
    }
    forget_client(&id);
    let removed = drop_keys(&mut OsKeychain, &id);
    forget_client(&id);
    removed
}

/// The secret first: a removal that stops halfway leaves the less sensitive half.
fn drop_keys(keys: &mut impl KeyStore, id: &str) -> Result<(), String> {
    keys.del(&kr_sec(id))?;
    keys.del(&kr_acc(id))
}

/// Adds or updates one connection and returns its id.
///
/// An empty `access_key` or `secret_key` leaves the stored key untouched, which
/// is how the UI can show a saved connection without ever holding its secrets;
/// a changed endpoint needs both keys again, and drops the stored ones first.
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
    if let Some(code) = crate::core::endpoint_problem(&endpoint) {
        return Err(code.into());
    }
    let mut conns = read_conns(&app);

    let cid = id
        .filter(|s| !s.is_empty())
        .or_else(|| active_id_inner(&app))
        .unwrap_or_else(|| "conn-1".to_string());

    let existing = conns.iter().find(|c| c.id == cid).cloned();
    // Stored keys stay with the endpoint they were saved for: a new endpoint
    // needs both keys typed again, or saving would send the old ones there.
    let typed = |k: &Option<String>| k.as_deref().is_some_and(|s| !s.trim().is_empty());
    let endpoint_changed = existing
        .as_ref()
        .is_some_and(|c| !crate::commands::same_endpoint(&c.endpoint, &endpoint));
    if endpoint_changed && !(typed(&access_key) && typed(&secret_key)) {
        return Err("keysRequired".into());
    }
    // Queued and running transfers find their connection by id when they run;
    // pointing that id at another endpoint or bucket would move them there.
    if let Some(c) = &existing
        && (c.endpoint != endpoint || c.bucket != bucket)
        && crate::transfers::connection_busy(&app, &cid)
    {
        return Err("busy".into());
    }
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
    let saved = commit_connection(
        &mut OsKeychain,
        &cid,
        endpoint_changed,
        access_key,
        secret_key,
        || {
            write_conns(&app, &conns)?;
            // Dropped before and after the keys are written; a command running in between may cache the old keys.
            forget_client(&cid);
            if read_active(&app).is_none() {
                write_active(&app, &cid)?;
            }
            Ok(())
        },
    );
    forget_client(&cid);
    saved.map(|()| cid)
}

/// The keychain as a save uses it, so a test can stand in for it with one
/// that refuses a write.
trait KeyStore {
    fn get(&self, account: &str) -> Option<String>;
    fn set(&mut self, account: &str, value: &str) -> Result<(), String>;
    fn del(&mut self, account: &str) -> Result<(), String>;
}

struct OsKeychain;

impl KeyStore for OsKeychain {
    fn get(&self, account: &str) -> Option<String> {
        kr_get(account)
    }
    fn set(&mut self, account: &str, value: &str) -> Result<(), String> {
        kr_set(account, value).map_err(|e| e.to_string())
    }
    fn del(&mut self, account: &str) -> Result<(), String> {
        kr_del(account)
    }
}

/// What a save writes, and in which order.
///
/// A changed endpoint loses its stored keys before the new address is written,
/// so the keys of one address are never held next to another: if the old keys
/// cannot be removed nothing changes, and if the new ones cannot be written the
/// connection is left without keys rather than with the old ones.
fn commit_connection(
    keys: &mut impl KeyStore,
    cid: &str,
    endpoint_changed: bool,
    access_key: Option<String>,
    secret_key: Option<String>,
    write_entry: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if endpoint_changed {
        // The secret first, as in a delete: a removal that stops halfway
        // leaves the less sensitive half behind.
        keys.del(&kr_sec(cid))?;
        keys.del(&kr_acc(cid))?;
    }
    write_entry()?;
    if let Some(ak) = access_key.filter(|s| !s.is_empty()) {
        keys.set(&kr_acc(cid), &ak)?;
    }
    if let Some(sk) = secret_key.filter(|s| !s.is_empty()) {
        keys.set(&kr_sec(cid), &sk)?;
    }
    Ok(())
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

/// The stored keys of one saved connection, for a test whose key fields were
/// left blank. Whether they may be used is `test_connection`'s decision.
pub fn stored_keys_for(id: &str) -> (Option<String>, Option<String>) {
    (kr_get(&kr_acc(id)), kr_get(&kr_sec(id)))
}

/// The endpoint a connection was saved with.
pub fn saved_endpoint(app: &AppHandle, id: &str) -> Option<String> {
    read_conns(app).into_iter().find(|c| c.id == id).map(|c| c.endpoint)
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
    // An endpoint saved before the https rule is refused here too. Only the code
    // travels on: the address itself may hold a name and password.
    if let Some(code) = crate::core::endpoint_problem(&c.endpoint) {
        return Err(code.into());
    }
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
    if let Some(code) = crate::core::endpoint_problem(&c.endpoint) {
        return Err(code.into());
    }
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

#[cfg(test)]
mod save_tests {
    use super::*;

    /// A keychain in memory that can be told to refuse the nth write, or every
    /// delete.
    #[derive(Default)]
    struct FakeKeys {
        held: HashMap<String, String>,
        writes: usize,
        refuse_write: Option<usize>,
        refuse_delete: bool,
    }

    impl FakeKeys {
        fn holding(id: &str, access: &str, secret: &str) -> Self {
            let mut k = Self::default();
            k.held.insert(kr_acc(id), access.into());
            k.held.insert(kr_sec(id), secret.into());
            k
        }
        fn pair(&self, id: &str) -> (Option<&str>, Option<&str>) {
            (
                self.held.get(&kr_acc(id)).map(String::as_str),
                self.held.get(&kr_sec(id)).map(String::as_str),
            )
        }
    }

    impl KeyStore for FakeKeys {
        fn get(&self, account: &str) -> Option<String> {
            self.held.get(account).cloned()
        }
        fn set(&mut self, account: &str, value: &str) -> Result<(), String> {
            self.writes += 1;
            if self.refuse_write == Some(self.writes) {
                return Err("the keychain refused the write".into());
            }
            self.held.insert(account.into(), value.into());
            Ok(())
        }
        fn del(&mut self, account: &str) -> Result<(), String> {
            if self.refuse_delete {
                return Err("the keychain refused the delete".into());
            }
            self.held.remove(account);
            Ok(())
        }
    }

    fn some(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn a_refused_key_write_does_not_leave_the_old_keys_on_a_new_endpoint() {
        for nth in [1, 2] {
            let mut keys = FakeKeys::holding("c", "old-access", "old-secret");
            keys.refuse_write = Some(nth);
            let mut moved = false;
            let out = commit_connection(&mut keys, "c", true, some("new-access"), some("new-secret"), || {
                moved = true;
                Ok(())
            });
            assert!(out.is_err(), "write {nth} was refused, the save must fail");
            let (access, secret) = keys.pair("c");
            assert!(
                !(moved && (access == Some("old-access") || secret == Some("old-secret"))),
                "write {nth} refused: the connection points at the new endpoint and still holds {access:?} / {secret:?}"
            );
        }
    }

    #[test]
    fn old_keys_that_cannot_be_removed_keep_the_connection_where_it_was() {
        let mut keys = FakeKeys::holding("c", "old-access", "old-secret");
        keys.refuse_delete = true;
        let mut moved = false;
        let out = commit_connection(&mut keys, "c", true, some("new-access"), some("new-secret"), || {
            moved = true;
            Ok(())
        });
        assert!(out.is_err());
        assert!(!moved, "the endpoint changed although the old keys are still stored");
        assert_eq!(keys.pair("c"), (Some("old-access"), Some("old-secret")));
    }

    #[test]
    fn a_changed_endpoint_ends_with_the_new_keys() {
        let mut keys = FakeKeys::holding("c", "old-access", "old-secret");
        let out = commit_connection(&mut keys, "c", true, some("new-access"), some("new-secret"), || Ok(()));
        assert!(out.is_ok());
        assert_eq!(keys.pair("c"), (Some("new-access"), Some("new-secret")));
    }

    #[test]
    fn the_same_endpoint_keeps_a_key_left_blank() {
        let mut keys = FakeKeys::holding("c", "old-access", "old-secret");
        let out = commit_connection(&mut keys, "c", false, None, some("new-secret"), || Ok(()));
        assert!(out.is_ok());
        assert_eq!(keys.pair("c"), (Some("old-access"), Some("new-secret")));
        let out = commit_connection(&mut keys, "c", false, some(""), None, || Ok(()));
        assert!(out.is_ok());
        assert_eq!(keys.pair("c"), (Some("old-access"), Some("new-secret")));
    }

    // ── The keys of the single-connection setup ──

    fn with_old_setup() -> FakeKeys {
        let mut k = FakeKeys::default();
        k.held.insert(KR_OLD_ACCESS.into(), "first-access".into());
        k.held.insert(KR_OLD_SECRET.into(), "first-secret".into());
        k
    }
    fn old_pair(k: &FakeKeys) -> (Option<&str>, Option<&str>) {
        (
            k.held.get(KR_OLD_ACCESS).map(String::as_str),
            k.held.get(KR_OLD_SECRET).map(String::as_str),
        )
    }

    #[test]
    fn the_old_setup_s_keys_move_to_the_first_connection_and_leave_nothing_behind() {
        let mut keys = with_old_setup();
        assert!(settle_legacy(&mut keys, Some("conn-1")).is_ok());
        assert_eq!(keys.pair("conn-1"), (Some("first-access"), Some("first-secret")));
        assert_eq!(old_pair(&keys), (None, None));
    }

    #[test]
    fn old_keys_do_not_replace_the_ones_the_connection_holds_now() {
        let mut keys = with_old_setup();
        keys.held.insert(kr_acc("conn-1"), "current-access".into());
        keys.held.insert(kr_sec("conn-1"), "current-secret".into());
        assert!(settle_legacy(&mut keys, Some("conn-1")).is_ok());
        assert_eq!(keys.pair("conn-1"), (Some("current-access"), Some("current-secret")));
        assert_eq!(old_pair(&keys), (None, None));
    }

    #[test]
    fn an_old_key_that_cannot_be_carried_over_stays_where_it_is() {
        let mut keys = with_old_setup();
        keys.refuse_write = Some(1);
        assert!(settle_legacy(&mut keys, Some("conn-1")).is_err());
        assert_eq!(old_pair(&keys), (Some("first-access"), Some("first-secret")));
    }

    #[test]
    fn removing_a_connection_s_keys_takes_both_and_touches_no_other() {
        let mut keys = FakeKeys::holding("c", "access", "secret");
        keys.held.insert(kr_acc("other"), "other-access".into());
        assert!(drop_keys(&mut keys, "c").is_ok());
        assert_eq!(keys.pair("c"), (None, None));
        assert_eq!(keys.pair("other"), (Some("other-access"), None));

        let mut keys = FakeKeys::holding("c", "access", "secret");
        keys.refuse_delete = true;
        assert!(drop_keys(&mut keys, "c").is_err());
    }

    #[test]
    fn old_keys_without_a_connection_to_take_them_are_removed() {
        let mut keys = with_old_setup();
        assert!(settle_legacy(&mut keys, None).is_ok());
        assert_eq!(old_pair(&keys), (None, None));
    }
}
