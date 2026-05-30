//! Multi-tenant authentication middleware.
//!
//! Extracts API key from the Authorization header, resolves the tenant,
//! and provides the correct Repository instance for each request.
//!
//! API keys are stored in a simple JSON file or Postgres table.
//! When auth is disabled (default for self-hosted), all requests use
//! the default single-tenant repository.

use std::collections::HashMap;
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
    pub can_migrate: bool,
    pub is_admin: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

/// A registered API key with its tenant and metadata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiKey {
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
    /// Optional namespace this key is scoped to. When set, the Repository
    /// is operated in this namespace for requests authenticated with this key.
    /// `None` means the server's configured default namespace applies.
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
    /// API key → tenant mapping
    keys: RwLock<HashMap<String, ApiKey>>,
    /// Tenant ID → Repository (cached, for future per-tenant storage)
    _repos: RwLock<HashMap<String, Arc<Repository>>>,
    /// Default repo (used when auth is disabled)
    default_repo: Arc<Repository>,
    /// Whether auth is enabled
    auth_enabled: bool,
    /// Path to API keys file (optional)
    keys_file: Option<String>,
}

impl TenantManager {
    /// Create a tenant manager with auth disabled (single-tenant mode).
    pub fn single_tenant(repo: Arc<Repository>) -> Arc<Self> {
        Arc::new(Self {
            keys: RwLock::new(HashMap::new()),
            _repos: RwLock::new(HashMap::new()),
            default_repo: repo,
            auth_enabled: false,
            keys_file: None,
        })
    }

    /// Create a tenant manager with auth enabled.
    /// Loads API keys from a JSON file if provided.
    pub fn multi_tenant(default_repo: Arc<Repository>, keys_file: Option<&str>) -> Arc<Self> {
        let mut keys = HashMap::new();

        if let Some(path) = keys_file
            && let Ok(data) = std::fs::read_to_string(path)
            && let Ok(loaded) = serde_json::from_str::<Vec<ApiKey>>(&data)
        {
            for key in loaded {
                keys.insert(key.key.clone(), key);
            }
        }

        Arc::new(Self {
            keys: RwLock::new(keys),
            _repos: RwLock::new(HashMap::new()),
            default_repo,
            auth_enabled: true,
            keys_file: keys_file.map(|s| s.to_string()),
        })
    }

    /// Register a new API key.
    pub fn register_key(&self, api_key: ApiKey) {
        if let Ok(mut keys) = self.keys.write() {
            keys.insert(api_key.key.clone(), api_key);
            self.save_keys();
        }
    }

    /// Generate a new API key for a tenant with default options.
    #[allow(dead_code)]
    pub fn create_key(&self, tenant_id: &str, name: &str, plan: &str) -> ApiKey {
        self.create_key_with(tenant_id, name, plan, CreateKeyOptions::default())
    }

    /// Generate a new API key with optional capability/meta fields (v3-V5).
    /// See [`CreateKeyOptions`] for field docs.
    pub fn create_key_with(
        &self,
        tenant_id: &str,
        name: &str,
        plan: &str,
        opts: CreateKeyOptions,
    ) -> ApiKey {
        let key = generate_key();
        let api_key = ApiKey {
            key: key.clone(),
            tenant_id: tenant_id.to_string(),
            name: name.to_string(),
            plan: plan.to_string(),
            enabled: true,
            created_at: Utc::now().to_rfc3339(),
            namespace_id: None,
            commit_agent_id: opts.commit_agent_id,
            can_migrate: opts.can_migrate,
            is_admin: opts.is_admin,
            expires_at: opts.expires_at,
            last_used_at: None,
        };
        self.register_key(api_key.clone());
        api_key
    }

    /// Rotate an API key: mint a new one with identical capabilities
    /// and expiry, and disable the old one (v3-V5). Returns the new
    /// `ApiKey` (the `.key` field is the only moment the raw value is
    /// exposed — handlers should return it once to the caller and then
    /// forget it). Returns `None` if no key matches `key_prefix`.
    pub fn rotate_key(&self, key_prefix: &str) -> Option<ApiKey> {
        // Snapshot the old key's settings first.
        let (
            tenant_id,
            name,
            plan,
            namespace_id,
            commit_agent_id,
            can_migrate,
            is_admin,
            expires_at,
        ) = {
            let keys = self.keys.read().ok()?;
            let full = keys.keys().find(|k| k.starts_with(key_prefix)).cloned()?;
            let old = keys.get(&full)?;
            (
                old.tenant_id.clone(),
                old.name.clone(),
                old.plan.clone(),
                old.namespace_id.clone(),
                old.commit_agent_id.clone(),
                old.can_migrate,
                old.is_admin,
                old.expires_at,
            )
        };
        // Mint the replacement, preserving all capabilities from the old key.
        let mut new_key = self.create_key_with(
            &tenant_id,
            &name,
            &plan,
            CreateKeyOptions {
                commit_agent_id,
                can_migrate,
                is_admin,
                expires_at,
            },
        );
        new_key.namespace_id = namespace_id;
        // Disable the old key.
        self.revoke_key(key_prefix);
        Some(new_key)
    }

    /// Register a raw `ApiKey` value (e.g. for operator-provided bootstrap
    /// admin key). Returns true if the key was inserted; false if a key
    /// with that same value already existed.
    pub fn register_admin_key(&self, key: String, name: &str) -> bool {
        let mut keys = match self.keys.write() {
            Ok(k) => k,
            Err(e) => e.into_inner(),
        };
        if keys.contains_key(&key) {
            return false;
        }
        keys.insert(
            key.clone(),
            ApiKey {
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
            },
        );
        drop(keys);
        self.save_keys();
        true
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
        let Some(key) = api_key else {
            return false;
        };
        let Ok(keys) = self.keys.read() else {
            return false;
        };
        keys.get(key)
            .map(|k| is_key_live(k) && k.is_admin)
            .unwrap_or(false)
    }

    /// Look up the authenticated agent_id bound to an API key, if any.
    pub fn get_agent_id(&self, api_key: &str) -> Option<String> {
        let keys = self.keys.read().ok()?;
        let k = keys.get(api_key)?;
        if !is_key_live(k) {
            return None;
        }
        k.commit_agent_id.clone()
    }

    /// Whether the given API key may commit with `IntentCategory::Migrate`.
    /// When auth is disabled (single-tenant mode), this returns true so
    /// local dev and self-hosted deployments are unaffected.
    pub fn can_migrate(&self, api_key: &str) -> bool {
        if !self.auth_enabled {
            return true;
        }
        let Ok(keys) = self.keys.read() else {
            return false;
        };
        keys.get(api_key)
            .map(|k| is_key_live(k) && k.can_migrate)
            .unwrap_or(false)
    }

    /// The namespace_id bound to this API key, if any.
    pub fn get_namespace_id(&self, api_key: &str) -> Option<String> {
        let Ok(keys) = self.keys.read() else {
            return None;
        };
        keys.get(api_key)
            .filter(|k| is_key_live(k))
            .and_then(|k| k.namespace_id.clone())
    }

    /// Whether auth is enabled on this manager.
    pub fn auth_enabled(&self) -> bool {
        self.auth_enabled
    }

    /// List all API keys (masked).
    pub fn list_keys(&self) -> Vec<serde_json::Value> {
        let keys = self.keys.read().unwrap_or_else(|e| e.into_inner());
        keys.values()
            .map(|k| {
                let masked = if k.key.len() > 12 {
                    format!("{}...{}", &k.key[..8], &k.key[k.key.len() - 4..])
                } else {
                    "***".to_string()
                };
                serde_json::json!({
                    "key_preview": masked,
                    "tenant_id": k.tenant_id,
                    "name": k.name,
                    "plan": k.plan,
                    "enabled": k.enabled,
                    "created_at": k.created_at,
                    "commit_agent_id": k.commit_agent_id,
                    "can_migrate": k.can_migrate,
                    "expires_at": k.expires_at.map(|t| t.to_rfc3339()),
                    "last_used_at": k.last_used_at.map(|t| t.to_rfc3339()),
                })
            })
            .collect()
    }

    /// Revoke an API key.
    pub fn revoke_key(&self, key_prefix: &str) -> bool {
        if let Ok(mut keys) = self.keys.write() {
            let matching: Vec<String> = keys
                .keys()
                .filter(|k| k.starts_with(key_prefix))
                .cloned()
                .collect();
            if let Some(key) = matching.first()
                && let Some(api_key) = keys.get_mut(key)
            {
                api_key.enabled = false;
                self.save_keys();
                return true;
            }
        }
        false
    }

    /// Look up tenant by API key. Treats expired keys identically to
    /// revoked/missing ones — same not-found response, so the reason
    /// is never leaked (v3-V5).
    pub fn resolve_tenant(&self, api_key: &str) -> Option<String> {
        // Peek under a read lock first; only take the write lock when
        // we actually need to update `last_used_at`.
        let (tenant, live) = {
            let keys = self.keys.read().ok()?;
            let key_info = keys.get(api_key)?;
            (key_info.tenant_id.clone(), is_key_live(key_info))
        };
        if !live {
            return None;
        }
        // Best-effort touch: don't fail resolution if we can't grab the
        // write lock (a poisoned lock shouldn't DoS auth).
        if let Ok(mut keys) = self.keys.write()
            && let Some(k) = keys.get_mut(api_key)
        {
            k.last_used_at = Some(Utc::now());
        }
        Some(tenant)
    }

    /// Get the repository for a request. If auth is disabled, returns the default repo.
    /// If auth is enabled, looks up the API key and returns the tenant's repo.
    pub fn get_repo(&self, api_key: Option<&str>) -> Result<Arc<Repository>, AuthError> {
        if !self.auth_enabled {
            return Ok(self.default_repo.clone());
        }

        let key = api_key.ok_or(AuthError::MissingKey)?;
        let _tenant_id = self.resolve_tenant(key).ok_or(AuthError::InvalidKey)?;

        // For now, all tenants share the same repo instance (Postgres tenant_id
        // handles isolation at the storage layer). In the future, we could
        // create per-tenant repos with different storage backends.
        Ok(self.default_repo.clone())
    }

    fn save_keys(&self) {
        if let Some(ref path) = self.keys_file
            && let Ok(keys) = self.keys.read()
        {
            let all: Vec<&ApiKey> = keys.values().collect();
            if let Ok(data) = serde_json::to_string_pretty(&all) {
                let _ = std::fs::write(path, data);
            }
        }
    }
}

/// Auth errors.
#[derive(Debug)]
pub enum AuthError {
    MissingKey,
    InvalidKey,
    NotAdmin,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AuthError::MissingKey => (
                StatusCode::UNAUTHORIZED,
                "Missing API key. Pass it as: Authorization: Bearer asg_...",
            ),
            AuthError::InvalidKey => (StatusCode::UNAUTHORIZED, "Invalid or revoked API key."),
            AuthError::NotAdmin => (
                StatusCode::FORBIDDEN,
                "Admin privilege required for this endpoint.",
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
/// expiry (v3-V5). Central helper so every validator agrees.
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
