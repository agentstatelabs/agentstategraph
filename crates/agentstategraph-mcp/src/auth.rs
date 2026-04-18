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

use agentstategraph::Repository;

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

    /// Generate a new API key for a tenant.
    #[allow(dead_code)]
    pub fn create_key(&self, tenant_id: &str, name: &str, plan: &str) -> ApiKey {
        self.create_key_with(tenant_id, name, plan, None, false, false)
    }

    /// Generate a new API key with optional commit_agent_id binding and
    /// migrate capability.
    pub fn create_key_with(
        &self,
        tenant_id: &str,
        name: &str,
        plan: &str,
        commit_agent_id: Option<String>,
        can_migrate: bool,
        is_admin: bool,
    ) -> ApiKey {
        let key = format!("asg_{}", uuid_v4());
        let api_key = ApiKey {
            key: key.clone(),
            tenant_id: tenant_id.to_string(),
            name: name.to_string(),
            plan: plan.to_string(),
            enabled: true,
            created_at: chrono::Utc::now().to_rfc3339(),
            commit_agent_id,
            can_migrate,
            is_admin,
        };
        self.register_key(api_key.clone());
        api_key
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
                created_at: chrono::Utc::now().to_rfc3339(),
                commit_agent_id: None,
                can_migrate: true,
                is_admin: true,
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
            .map(|k| k.enabled && k.is_admin)
            .unwrap_or(false)
    }

    /// Look up the authenticated agent_id bound to an API key, if any.
    pub fn get_agent_id(&self, api_key: &str) -> Option<String> {
        let keys = self.keys.read().ok()?;
        let k = keys.get(api_key)?;
        if !k.enabled {
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
            .map(|k| k.enabled && k.can_migrate)
            .unwrap_or(false)
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

    /// Look up tenant by API key.
    pub fn resolve_tenant(&self, api_key: &str) -> Option<String> {
        let keys = self.keys.read().ok()?;
        let key_info = keys.get(api_key)?;
        if key_info.enabled {
            Some(key_info.tenant_id.clone())
        } else {
            None
        }
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
        }
    } else {
        AuthContext {
            commit_agent_id: None,
            can_migrate: true,
            key_prefix: None,
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

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")
}
