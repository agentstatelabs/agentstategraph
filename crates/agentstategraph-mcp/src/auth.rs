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
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use agentstategraph::Repository;

/// A registered API key with its tenant and metadata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiKey {
    pub key: String,
    pub tenant_id: String,
    pub name: String,
    pub plan: String,      // "free", "standard", "enterprise"
    pub enabled: bool,
    pub created_at: String,
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
    pub fn multi_tenant(
        default_repo: Arc<Repository>,
        keys_file: Option<&str>,
    ) -> Arc<Self> {
        let mut keys = HashMap::new();

        if let Some(path) = keys_file {
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(loaded) = serde_json::from_str::<Vec<ApiKey>>(&data) {
                    for key in loaded {
                        keys.insert(key.key.clone(), key);
                    }
                }
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
    pub fn create_key(&self, tenant_id: &str, name: &str, plan: &str) -> ApiKey {
        let key = format!("asg_{}", uuid_v4());
        let api_key = ApiKey {
            key: key.clone(),
            tenant_id: tenant_id.to_string(),
            name: name.to_string(),
            plan: plan.to_string(),
            enabled: true,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.register_key(api_key.clone());
        api_key
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
            if let Some(key) = matching.first() {
                if let Some(api_key) = keys.get_mut(key) {
                    api_key.enabled = false;
                    self.save_keys();
                    return true;
                }
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
        if let Some(ref path) = self.keys_file {
            if let Ok(keys) = self.keys.read() {
                let all: Vec<&ApiKey> = keys.values().collect();
                if let Ok(data) = serde_json::to_string_pretty(&all) {
                    let _ = std::fs::write(path, data);
                }
            }
        }
    }
}

/// Auth errors.
#[derive(Debug)]
pub enum AuthError {
    MissingKey,
    InvalidKey,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AuthError::MissingKey => (
                StatusCode::UNAUTHORIZED,
                "Missing API key. Pass it as: Authorization: Bearer asg_...",
            ),
            AuthError::InvalidKey => (
                StatusCode::UNAUTHORIZED,
                "Invalid or revoked API key.",
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

    // Inject the resolved repo into request extensions so handlers can access it
    request.extensions_mut().insert(repo);

    Ok(next.run(request).await)
}

/// Extract API key from Authorization header or query parameter.
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    // Try Authorization: Bearer <key>
    if let Some(auth) = headers.get("authorization") {
        if let Ok(value) = auth.to_str() {
            if let Some(key) = value.strip_prefix("Bearer ") {
                return Some(key.trim().to_string());
            }
            // Also accept plain key without "Bearer"
            if value.starts_with("asg_") {
                return Some(value.trim().to_string());
            }
        }
    }

    // Try X-API-Key header
    if let Some(key) = headers.get("x-api-key") {
        if let Ok(value) = key.to_str() {
            return Some(value.trim().to_string());
        }
    }

    None
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")
}
