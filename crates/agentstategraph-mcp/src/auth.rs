//! Multi-tenant authentication middleware.
//!
//! Extracts API key from the Authorization header, resolves the tenant,
//! and provides the correct Repository instance for each request.
//!
//! API keys are kept in a JSON file (`--keys-file`), as SHA-256 hashes: the
//! raw key is shown once, when it is created, and never stored. Each key has
//! a stable id that admin calls use to revoke or rotate it. The file is
//! written atomically with owner-only permissions, and a file that can't be
//! read stops the server rather than being replaced. When auth is disabled
//! (default for self-hosted), all requests use the default single-tenant
//! repository.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use rand::{RngCore, TryRngCore};

use agentstategraph::Repository;

/// Optional capability/meta fields for [`TenantManager::create_key_with`].
#[derive(Default)]
pub struct CreateKeyOptions {
    pub commit_agent_id: Option<String>,
    /// Bind the key to this namespace (see [`ApiKey::namespace_id`]).
    pub namespace_id: Option<String>,
    pub can_migrate: bool,
    pub is_admin: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

/// An API key with its raw value, as handed to a caller when it is created
/// or registered. Only its hash is kept (see [`StoredKey`]).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiKey {
    /// Stable id for admin calls (revoke, rotate). Empty when registering a
    /// key that doesn't have one yet; one is assigned.
    #[serde(default)]
    pub id: String,
    pub key: String,
    pub tenant_id: String,
    pub name: String,
    pub plan: String, // "free", "standard", "enterprise"
    pub enabled: bool,
    pub created_at: String,
    /// Optional authenticated agent identity bound to this key.
    /// When set, commits via HTTP must use this as the `agent_id`,
    /// ignoring any body-supplied `agent` field (§8 — authenticated
    /// commit agent_id).
    #[serde(default)]
    pub commit_agent_id: Option<String>,
    /// Optional namespace this key is bound to. Requests with it work in that
    /// namespace and name refs by branch only: commit ids reach across
    /// namespaces, and store-wide endpoints (GC, history) refuse it. `None`
    /// means the server's configured default namespace, with no limits.
    #[serde(default)]
    pub namespace_id: Option<String>,
    /// Whether this key is allowed to issue commits tagged
    /// `IntentCategory::Migrate` (§10 — key-scoped migrate capability).
    /// Defaults to false; only trusted keys should have it.
    #[serde(default)]
    pub can_migrate: bool,
    /// Whether this key is authorized to call `/api/admin/*` endpoints
    /// (v2-C1). Defaults to false; only the bootstrap admin key and any
    /// additional keys explicitly provisioned by an admin have it.
    #[serde(default)]
    pub is_admin: bool,
    /// Optional expiry. When set and `Utc::now() > expires_at`, the key
    /// is treated as revoked (v3-V5). `None` means never expires.
    /// Serde default so existing on-disk keys continue to deserialize.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Last time this key successfully validated. Nice-to-have for
    /// lifecycle dashboards (v3-V5).
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
}

/// A key as the server keeps it: metadata and a hash of the key, never the
/// key itself.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredKey {
    pub id: String,
    /// Lowercase hex SHA-256 of the raw key.
    pub key_hash: String,
    /// The key's first 8 and last 4 characters, for telling keys apart.
    pub key_preview: String,
    pub tenant_id: String,
    pub name: String,
    pub plan: String,
    pub enabled: bool,
    pub created_at: String,
    #[serde(default)]
    pub commit_agent_id: Option<String>,
    #[serde(default)]
    pub namespace_id: Option<String>,
    #[serde(default)]
    pub can_migrate: bool,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
}

impl StoredKey {
    /// The stored form of `key`, with `id` if it has none.
    fn from_api_key(key: &ApiKey, id: String) -> Self {
        Self {
            id,
            key_hash: hash_key(&key.key),
            key_preview: preview(&key.key),
            tenant_id: key.tenant_id.clone(),
            name: key.name.clone(),
            plan: key.plan.clone(),
            enabled: key.enabled,
            created_at: key.created_at.clone(),
            commit_agent_id: key.commit_agent_id.clone(),
            namespace_id: key.namespace_id.clone(),
            can_migrate: key.can_migrate,
            is_admin: key.is_admin,
            expires_at: key.expires_at,
            last_used_at: key.last_used_at,
        }
    }

    /// This key with its raw value, to hand back to the caller once.
    fn issue(&self, raw: String) -> ApiKey {
        ApiKey {
            id: self.id.clone(),
            key: raw,
            tenant_id: self.tenant_id.clone(),
            name: self.name.clone(),
            plan: self.plan.clone(),
            enabled: self.enabled,
            created_at: self.created_at.clone(),
            commit_agent_id: self.commit_agent_id.clone(),
            namespace_id: self.namespace_id.clone(),
            can_migrate: self.can_migrate,
            is_admin: self.is_admin,
            expires_at: self.expires_at,
            last_used_at: self.last_used_at,
        }
    }

    fn is_live(&self) -> bool {
        self.enabled && self.expires_at.is_none_or(|exp| Utc::now() <= exp)
    }
}

/// Version of the keys file this build writes. Version 1 was a bare array of
/// [`ApiKey`] with raw keys; it is read and rewritten as version 2.
const KEYS_FILE_VERSION: u32 = 2;

#[derive(serde::Serialize, serde::Deserialize)]
struct KeysFile {
    version: u32,
    keys: Vec<StoredKey>,
}

/// The keys file couldn't be read, parsed or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStoreError(pub String);

impl std::fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for KeyStoreError {}

/// Which key an admin call means: its id, or the key itself. Never a prefix,
/// which can match more than one key.
pub enum KeyRef<'a> {
    Id(&'a str),
    Key(&'a str),
}

/// Context resolved by the auth middleware for each request.
/// Injected into `request.extensions_mut()` so handlers can reach it.
#[derive(Debug, Clone, Default)]
pub struct AuthContext {
    /// Authenticated agent_id bound to the key, if any.
    pub commit_agent_id: Option<String>,
    /// Whether this request is allowed to commit with `IntentCategory::Migrate`.
    pub can_migrate: bool,
    /// Short prefix of the key (for logging), if any.
    pub key_prefix: Option<String>,
    /// Namespace from the API key, if any.
    pub namespace_id: Option<String>,
}

/// Manages tenants, API keys, and per-tenant Repository instances.
pub struct TenantManager {
    /// Key hash → key.
    keys: RwLock<HashMap<String, StoredKey>>,
    /// Namespace → the repository serving keys bound to it: a fork of the
    /// default repository, limited to branch names.
    namespace_repos: RwLock<HashMap<String, Arc<Repository>>>,
    /// Default repo (used when auth is disabled)
    default_repo: Arc<Repository>,
    /// The tenant this process serves (`--tenant`). A non-admin key for
    /// another tenant is refused: its data isn't here, and quietly serving
    /// this tenant's data to it would cross tenants.
    tenant_id: String,
    /// Whether auth is enabled
    auth_enabled: bool,
    /// Path to API keys file (optional)
    keys_file: Option<PathBuf>,
}

impl TenantManager {
    /// Create a tenant manager with auth disabled (single-tenant mode).
    pub fn single_tenant(repo: Arc<Repository>) -> Arc<Self> {
        Arc::new(Self {
            keys: RwLock::new(HashMap::new()),
            namespace_repos: RwLock::new(HashMap::new()),
            default_repo: repo,
            tenant_id: DEFAULT_TENANT.to_string(),
            auth_enabled: false,
            keys_file: None,
        })
    }

    /// Create a tenant manager with auth enabled, loading keys from
    /// `keys_file` if given (a missing file is an empty one).
    ///
    /// Fails if the file can't be read or parsed, rather than starting
    /// without its keys and later overwriting it. A version 1 file (raw keys)
    /// is hashed and rewritten in place; if it can't be rewritten (a
    /// read-only mount, say), the server still starts, with a warning.
    pub fn multi_tenant(
        default_repo: Arc<Repository>,
        keys_file: Option<&str>,
    ) -> Result<Arc<Self>, KeyStoreError> {
        Self::multi_tenant_for(default_repo, keys_file, DEFAULT_TENANT)
    }

    /// [`TenantManager::multi_tenant`] for a process serving `tenant_id`
    /// (`--tenant`). Non-admin keys must belong to it.
    pub fn multi_tenant_for(
        default_repo: Arc<Repository>,
        keys_file: Option<&str>,
        tenant_id: &str,
    ) -> Result<Arc<Self>, KeyStoreError> {
        let keys_file = keys_file.map(PathBuf::from);
        let (keys, migrated) = match &keys_file {
            Some(path) => load_keys(path)?,
            None => (HashMap::new(), false),
        };
        let mgr = Arc::new(Self {
            keys: RwLock::new(keys),
            namespace_repos: RwLock::new(HashMap::new()),
            default_repo,
            tenant_id: tenant_id.to_string(),
            auth_enabled: true,
            keys_file,
        });
        if let Some(path) = &mgr.keys_file {
            if migrated {
                let keys = mgr.keys.read().unwrap_or_else(|e| e.into_inner());
                match write_keys_file(path, &keys) {
                    Ok(()) => tracing::info!(
                        path = %path.display(),
                        keys = keys.len(),
                        "keys file now stores hashes instead of keys"
                    ),
                    Err(e) => eprintln!(
                        "WARNING: {}: still holds raw keys, and couldn't be rewritten with \
                         hashes ({e}). Keys work; fix the file's permissions so it can be.",
                        path.display()
                    ),
                }
            }
            warn_if_readable_by_others(path);
        }
        Ok(mgr)
    }

    /// Apply `change` to the keys and save them. Nothing changes in memory
    /// unless the save succeeds, so an admin call never reports a change the
    /// file doesn't have (a revoke that would come back after a restart).
    fn update<T>(
        &self,
        change: impl FnOnce(&mut HashMap<String, StoredKey>) -> T,
    ) -> Result<T, KeyStoreError> {
        let mut keys = self.keys.write().unwrap_or_else(|e| e.into_inner());
        let mut next = keys.clone();
        let out = change(&mut next);
        if let Some(path) = &self.keys_file {
            write_keys_file(path, &next)?;
        }
        *keys = next;
        Ok(out)
    }

    /// The live stored key for a raw key.
    fn live(&self, api_key: &str) -> Option<StoredKey> {
        let keys = self.keys.read().ok()?;
        keys.get(&hash_key(api_key))
            .filter(|k| k.is_live())
            .cloned()
    }

    /// Register a key (its hash). Assigns an id if it has none; returns it.
    pub fn register_key(&self, api_key: ApiKey) -> Result<String, KeyStoreError> {
        let id = if api_key.id.is_empty() {
            generate_key_id()
        } else {
            api_key.id.clone()
        };
        let stored = StoredKey::from_api_key(&api_key, id.clone());
        self.update(|keys| {
            keys.insert(stored.key_hash.clone(), stored);
        })?;
        Ok(id)
    }

    /// Generate a new API key for a tenant with default options.
    #[allow(dead_code)]
    pub fn create_key(
        &self,
        tenant_id: &str,
        name: &str,
        plan: &str,
    ) -> Result<ApiKey, KeyStoreError> {
        self.create_key_with(tenant_id, name, plan, CreateKeyOptions::default())
    }

    /// Generate a new API key with optional capability/meta fields (v3-V5).
    /// See [`CreateKeyOptions`] for field docs. The returned key is the only
    /// place its raw value appears.
    pub fn create_key_with(
        &self,
        tenant_id: &str,
        name: &str,
        plan: &str,
        opts: CreateKeyOptions,
    ) -> Result<ApiKey, KeyStoreError> {
        let mut api_key = ApiKey {
            id: generate_key_id(),
            key: generate_key(),
            tenant_id: tenant_id.to_string(),
            name: name.to_string(),
            plan: plan.to_string(),
            enabled: true,
            created_at: Utc::now().to_rfc3339(),
            namespace_id: opts.namespace_id,
            commit_agent_id: opts.commit_agent_id,
            can_migrate: opts.can_migrate,
            is_admin: opts.is_admin,
            expires_at: opts.expires_at,
            last_used_at: None,
        };
        api_key.id = self.register_key(api_key.clone())?;
        Ok(api_key)
    }

    /// The id of the key `key_ref` names, if one is registered.
    pub fn key_id(&self, key_ref: KeyRef<'_>) -> Option<String> {
        let keys = self.keys.read().ok()?;
        match key_ref {
            KeyRef::Id(id) => keys.values().find(|k| k.id == id).map(|k| k.id.clone()),
            KeyRef::Key(raw) => keys.get(&hash_key(raw)).map(|k| k.id.clone()),
        }
    }

    /// Rotate an API key: mint a new one with the same tenant, name,
    /// capabilities, namespace and expiry, and disable the old one, in one
    /// save (v3-V5). Returns the new key, whose `.key` is the only time the
    /// raw value is exposed, or `None` if no key has id `key_id`.
    pub fn rotate_key(&self, key_id: &str) -> Result<Option<ApiKey>, KeyStoreError> {
        let raw = generate_key();
        let new_id = generate_key_id();
        let rotated = self.update(|keys| {
            let old = keys.values_mut().find(|k| k.id == key_id)?;
            old.enabled = false;
            let mut new = old.clone();
            new.id = new_id;
            new.key_hash = hash_key(&raw);
            new.key_preview = preview(&raw);
            new.enabled = true;
            new.created_at = Utc::now().to_rfc3339();
            new.last_used_at = None;
            keys.insert(new.key_hash.clone(), new.clone());
            Some(new)
        })?;
        Ok(rotated.map(|new| new.issue(raw)))
    }

    /// Register an operator-provided key (the bootstrap admin key). Returns
    /// true if it was added, false if it was already registered.
    pub fn register_admin_key(&self, key: String, name: &str) -> Result<bool, KeyStoreError> {
        if self.key_id(KeyRef::Key(&key)).is_some() {
            return Ok(false);
        }
        self.register_key(ApiKey {
            id: String::new(),
            key,
            tenant_id: "admin".to_string(),
            name: name.to_string(),
            plan: "admin".to_string(),
            enabled: true,
            created_at: Utc::now().to_rfc3339(),
            namespace_id: None,
            commit_agent_id: None,
            can_migrate: true,
            is_admin: true,
            expires_at: None,
            last_used_at: None,
        })?;
        Ok(true)
    }

    /// Whether any enabled admin key is registered.
    pub fn has_admin_key(&self) -> bool {
        let Ok(keys) = self.keys.read() else {
            return false;
        };
        keys.values().any(|k| k.enabled && k.is_admin)
    }

    /// Whether the given key is a valid admin. When auth is disabled
    /// (single-tenant mode), admin endpoints are accessible without any
    /// key — single-tenant == trusted local process.
    pub fn is_admin(&self, api_key: Option<&str>) -> bool {
        if !self.auth_enabled {
            return true;
        }
        api_key
            .and_then(|k| self.live(k))
            .is_some_and(|k| k.is_admin)
    }

    /// Look up the authenticated agent_id bound to an API key, if any.
    pub fn get_agent_id(&self, api_key: &str) -> Option<String> {
        self.live(api_key)?.commit_agent_id
    }

    /// Whether the given API key may commit with `IntentCategory::Migrate`.
    /// When auth is disabled (single-tenant mode), this returns true so
    /// local dev and self-hosted deployments are unaffected.
    pub fn can_migrate(&self, api_key: &str) -> bool {
        if !self.auth_enabled {
            return true;
        }
        self.live(api_key).is_some_and(|k| k.can_migrate)
    }

    /// The namespace_id bound to this API key, if any.
    pub fn get_namespace_id(&self, api_key: &str) -> Option<String> {
        self.live(api_key)?.namespace_id
    }

    /// Whether auth is enabled on this manager.
    pub fn auth_enabled(&self) -> bool {
        self.auth_enabled
    }

    /// List all API keys: ids, previews and metadata, never keys or hashes.
    pub fn list_keys(&self) -> Vec<serde_json::Value> {
        let keys = self.keys.read().unwrap_or_else(|e| e.into_inner());
        let mut listed: Vec<&StoredKey> = keys.values().collect();
        listed.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        listed
            .into_iter()
            .map(|k| {
                serde_json::json!({
                    "id": k.id,
                    "key_preview": k.key_preview,
                    "tenant_id": k.tenant_id,
                    "name": k.name,
                    "plan": k.plan,
                    "enabled": k.enabled,
                    "created_at": k.created_at,
                    "commit_agent_id": k.commit_agent_id,
                    "namespace_id": k.namespace_id,
                    "can_migrate": k.can_migrate,
                    "is_admin": k.is_admin,
                    "expires_at": k.expires_at.map(|t| t.to_rfc3339()),
                    "last_used_at": k.last_used_at.map(|t| t.to_rfc3339()),
                })
            })
            .collect()
    }

    /// Disable the key with id `key_id`. Returns false if there is none.
    pub fn revoke_key(&self, key_id: &str) -> Result<bool, KeyStoreError> {
        self.update(|keys| {
            keys.values_mut()
                .find(|k| k.id == key_id)
                .map(|k| k.enabled = false)
                .is_some()
        })
    }

    /// Look up tenant by API key. Treats expired keys identically to
    /// revoked/missing ones — same not-found response, so the reason
    /// is never leaked (v3-V5).
    pub fn resolve_tenant(&self, api_key: &str) -> Option<String> {
        let tenant = self.live(api_key)?.tenant_id;
        // Best-effort touch, in memory only: it's saved with the next
        // change. A poisoned lock mustn't block auth.
        if let Ok(mut keys) = self.keys.write()
            && let Some(k) = keys.get_mut(&hash_key(api_key))
        {
            k.last_used_at = Some(Utc::now());
        }
        Some(tenant)
    }

    /// The tenant this process serves.
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// The repository a request works in. With auth disabled, the default
    /// repository. With auth enabled, the key must be live and, unless it is
    /// an admin key, belong to this process's tenant; a key bound to a
    /// namespace gets a repository in that namespace, limited to branch
    /// names.
    pub fn get_repo(&self, api_key: Option<&str>) -> Result<Arc<Repository>, AuthError> {
        if !self.auth_enabled {
            return Ok(self.default_repo.clone());
        }

        let key = api_key.ok_or(AuthError::MissingKey)?;
        let tenant = self.resolve_tenant(key).ok_or(AuthError::InvalidKey)?;
        let stored = self.live(key).ok_or(AuthError::InvalidKey)?;
        if !stored.is_admin && tenant != self.tenant_id {
            return Err(AuthError::WrongTenant {
                key_tenant: tenant,
                server_tenant: self.tenant_id.clone(),
            });
        }
        match stored.namespace_id.as_deref() {
            None => Ok(self.default_repo.clone()),
            Some(ns) => self.namespace_repo(ns),
        }
    }

    /// The (cached) branch-names-only fork of the default repository in
    /// namespace `ns`.
    fn namespace_repo(&self, ns: &str) -> Result<Arc<Repository>, AuthError> {
        if let Some(repo) = self
            .namespace_repos
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(ns)
        {
            return Ok(Arc::clone(repo));
        }
        let namespace =
            agentstategraph_core::Namespace::new(ns).map_err(|_| AuthError::BadKeyNamespace)?;
        let repo = Arc::new(
            self.default_repo
                .fork_namespace(namespace)
                .with_branch_refs_only(),
        );
        Ok(Arc::clone(
            self.namespace_repos
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entry(ns.to_string())
                .or_insert(repo),
        ))
    }
}

/// Read the keys file: `(keys by hash, whether it held raw keys)`. A missing
/// file is empty; anything unreadable is an error.
fn load_keys(path: &Path) -> Result<(HashMap<String, StoredKey>, bool), KeyStoreError> {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((HashMap::new(), false));
        }
        Err(e) => {
            return Err(KeyStoreError(format!(
                "can't read keys file {}: {e}",
                path.display()
            )));
        }
    };
    let unreadable = |what: &str, e: serde_json::Error| {
        KeyStoreError(format!(
            "keys file {} isn't a valid {what}: {e}. Fix or move it; the server won't \
             start without its keys, or replace it.",
            path.display()
        ))
    };
    if data.trim_start().starts_with('[') {
        // Version 1: raw keys.
        let legacy: Vec<ApiKey> =
            serde_json::from_str(&data).map_err(|e| unreadable("version 1 keys file", e))?;
        let keys = legacy
            .iter()
            .map(|k| {
                let id = if k.id.is_empty() {
                    generate_key_id()
                } else {
                    k.id.clone()
                };
                let stored = StoredKey::from_api_key(k, id);
                (stored.key_hash.clone(), stored)
            })
            .collect();
        return Ok((keys, true));
    }
    let file: KeysFile = serde_json::from_str(&data).map_err(|e| unreadable("keys file", e))?;
    if file.version > KEYS_FILE_VERSION {
        return Err(KeyStoreError(format!(
            "keys file {} is version {}, written by a newer server; this one reads up to {}",
            path.display(),
            file.version,
            KEYS_FILE_VERSION
        )));
    }
    Ok((
        file.keys
            .into_iter()
            .map(|k| (k.key_hash.clone(), k))
            .collect(),
        false,
    ))
}

/// Replace the keys file: write a temp file beside it, owner-only, flush it
/// to disk, then rename it over the old one. A crash leaves the old file or
/// the new one, never a partial one.
fn write_keys_file(path: &Path, keys: &HashMap<String, StoredKey>) -> Result<(), KeyStoreError> {
    use std::io::Write;
    let failed = |what: &str, e: &dyn std::fmt::Display| {
        KeyStoreError(format!(
            "couldn't save keys file {} ({what}): {e}",
            path.display()
        ))
    };
    let mut sorted: Vec<StoredKey> = keys.values().cloned().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let data = serde_json::to_vec_pretty(&KeysFile {
        version: KEYS_FILE_VERSION,
        keys: sorted,
    })
    .map_err(|e| failed("serialize", &e))?;

    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "keys".into());
    let tmp = dir.join(format!(".{name}.{}.tmp", &generate_key_id()[4..]));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&tmp).and_then(|mut f| {
        f.write_all(&data)?;
        f.sync_all()
    });
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(failed("write", &e));
    }
    // Make the rename itself durable. Best effort: not every platform can.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(unix)]
fn warn_if_readable_by_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path)
        && meta.permissions().mode() & 0o077 != 0
    {
        eprintln!(
            "WARNING: keys file {} can be read by other users (mode {:o}); it is \
             tightened to owner-only the next time keys change.",
            path.display(),
            meta.permissions().mode() & 0o777
        );
    }
}

#[cfg(not(unix))]
fn warn_if_readable_by_others(_path: &Path) {}

/// Lowercase hex SHA-256 of a raw key. API keys carry 256 random bits, so a
/// plain hash keeps them safe if the file leaks; no slow hash is needed.
fn hash_key(raw: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(raw.as_bytes()))
}

/// The first 8 and last 4 characters of a key, or `***` for a short one.
fn preview(raw: &str) -> String {
    if raw.len() > 12 && raw.is_char_boundary(8) && raw.is_char_boundary(raw.len() - 4) {
        format!("{}...{}", &raw[..8], &raw[raw.len() - 4..])
    } else {
        "***".to_string()
    }
}

/// A fresh key id: `key_` and 16 random hex characters.
fn generate_key_id() -> String {
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    format!("key_{}", hex::encode(bytes))
}

/// The tenant a process serves unless `--tenant` says otherwise.
pub const DEFAULT_TENANT: &str = "default";

/// Auth errors.
#[derive(Debug)]
pub enum AuthError {
    MissingKey,
    InvalidKey,
    NotAdmin,
    /// A non-admin key for a tenant this process doesn't serve.
    WrongTenant {
        key_tenant: String,
        server_tenant: String,
    },
    /// The key's namespace isn't a valid namespace name.
    BadKeyNamespace,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AuthError::MissingKey => (
                StatusCode::UNAUTHORIZED,
                "Missing API key. Pass it as: Authorization: Bearer asg_...".to_string(),
            ),
            AuthError::InvalidKey => (
                StatusCode::UNAUTHORIZED,
                "Invalid or revoked API key.".to_string(),
            ),
            AuthError::NotAdmin => (
                StatusCode::FORBIDDEN,
                "Admin privilege required for this endpoint.".to_string(),
            ),
            AuthError::WrongTenant {
                key_tenant,
                server_tenant,
            } => (
                StatusCode::FORBIDDEN,
                format!(
                    "This key belongs to tenant '{key_tenant}', but this server serves tenant \
                     '{server_tenant}'. Use a key for '{server_tenant}', or start the server \
                     with --tenant {key_tenant}."
                ),
            ),
            AuthError::BadKeyNamespace => (
                StatusCode::FORBIDDEN,
                "This key is bound to an invalid namespace name.".to_string(),
            ),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

/// Axum middleware that extracts the API key and validates the tenant.
pub async fn auth_middleware(
    State(tenant_mgr): State<Arc<TenantManager>>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, AuthError> {
    let api_key = extract_api_key(&headers);
    let repo = tenant_mgr.get_repo(api_key.as_deref())?;

    // Build AuthContext for handlers: carries authenticated agent_id and
    // migrate capability. When auth is disabled, `can_migrate` defaults
    // to true (single-tenant dev) and `commit_agent_id` stays None.
    let ctx = if tenant_mgr.auth_enabled() {
        let commit_agent_id = api_key.as_deref().and_then(|k| tenant_mgr.get_agent_id(k));
        let can_migrate = api_key
            .as_deref()
            .map(|k| tenant_mgr.can_migrate(k))
            .unwrap_or(false);
        let namespace_id = api_key
            .as_deref()
            .and_then(|k| tenant_mgr.get_namespace_id(k));
        let key_prefix = api_key.as_deref().map(|k| {
            if k.len() > 8 {
                k[..8].to_string()
            } else {
                k.to_string()
            }
        });
        AuthContext {
            commit_agent_id,
            can_migrate,
            key_prefix,
            namespace_id,
        }
    } else {
        AuthContext {
            commit_agent_id: None,
            can_migrate: true,
            key_prefix: None,
            namespace_id: None,
        }
    };

    // Inject the resolved repo and auth context into request extensions
    // so handlers can access them.
    request.extensions_mut().insert(repo);
    request.extensions_mut().insert(ctx);

    Ok(next.run(request).await)
}

/// Axum middleware for `/api/admin/*` endpoints.
///
/// Uses the same `extract_api_key` path as `auth_middleware` and rejects
/// any request whose key is not flagged `is_admin`. In single-tenant
/// mode (auth disabled) the request passes through — that matches the
/// `can_migrate` affordance we already have for trusted local processes.
pub async fn admin_auth_middleware(
    State(tenant_mgr): State<Arc<TenantManager>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, AuthError> {
    // Single-tenant mode: admin endpoints are accessible without a key.
    if !tenant_mgr.auth_enabled() {
        return Ok(next.run(request).await);
    }

    let api_key = extract_api_key(&headers);
    let Some(key) = api_key.as_deref() else {
        return Err(AuthError::MissingKey);
    };
    if tenant_mgr.resolve_tenant(key).is_none() {
        return Err(AuthError::InvalidKey);
    }
    if !tenant_mgr.is_admin(Some(key)) {
        return Err(AuthError::NotAdmin);
    }
    Ok(next.run(request).await)
}

/// Extract API key from Authorization header or query parameter.
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    // Try Authorization: Bearer <key>
    if let Some(auth) = headers.get("authorization")
        && let Ok(value) = auth.to_str()
    {
        if let Some(key) = value.strip_prefix("Bearer ") {
            return Some(key.trim().to_string());
        }
        // Also accept plain key without "Bearer"
        if value.starts_with("asg_") {
            return Some(value.trim().to_string());
        }
    }

    // Try X-API-Key header
    if let Some(key) = headers.get("x-api-key")
        && let Ok(value) = key.to_str()
    {
        return Some(value.trim().to_string());
    }

    None
}

/// Whether a key is currently usable: enabled and not past its
/// expiry (v3-V5).
#[cfg(test)]
fn is_key_live(k: &ApiKey) -> bool {
    if !k.enabled {
        return false;
    }
    if let Some(exp) = k.expires_at
        && Utc::now() > exp
    {
        return false;
    }
    true
}

/// Generate a fresh API key with 256 bits of entropy from the OS RNG,
/// hex-encoded and prefixed with `asg_` (v3-V5). Format:
/// `asg_<64 hex chars>`.
fn generate_key() -> String {
    let mut bytes = [0u8; 32];
    // Prefer the OS RNG; if it's unavailable for any reason, fall back
    // to the thread RNG (ChaCha, seeded from OS entropy at thread
    // start). Either path is cryptographically strong.
    if rand::rngs::OsRng.try_fill_bytes(&mut bytes).is_err() {
        rand::rng().fill_bytes(&mut bytes);
    }
    let mut s = String::with_capacity(4 + 64);
    s.push_str("asg_");
    for b in &bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_matches_asg_hex64() {
        let k = generate_key();
        assert!(k.starts_with("asg_"), "missing asg_ prefix: {}", k);
        let hex = &k[4..];
        assert_eq!(hex.len(), 64, "hex must be 64 chars: {}", k);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "hex must be lowercase: {}",
            k
        );
    }

    #[test]
    fn generated_keys_are_unique() {
        let a = generate_key();
        let b = generate_key();
        assert_ne!(a, b);
    }

    #[test]
    fn is_key_live_respects_enabled_and_expiry() {
        let mut k = ApiKey {
            id: String::new(),
            key: "asg_x".into(),
            tenant_id: "t".into(),
            name: "n".into(),
            plan: "free".into(),
            enabled: true,
            created_at: Utc::now().to_rfc3339(),
            namespace_id: None,
            commit_agent_id: None,
            can_migrate: false,
            is_admin: false,
            expires_at: None,
            last_used_at: None,
        };
        assert!(is_key_live(&k));
        k.enabled = false;
        assert!(!is_key_live(&k));
        k.enabled = true;
        k.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
        assert!(!is_key_live(&k));
        k.expires_at = Some(Utc::now() + chrono::Duration::hours(1));
        assert!(is_key_live(&k));
    }
}
