//! [`HttpRemote`]: a [`Remote`] on the other end of `/sync/v1` (DESIGN.md
//! Plan G t-006).
//!
//! Its calls block, as [`Remote`]'s do, so an async caller runs them on a
//! blocking thread. HTTPS uses rustls with the Mozilla root certificates.

use std::io::Read;
use std::time::Duration;

use agentstategraph::sync::{
    BundleLimits, BundleRef, FetchRequest, PushOutcome, RefUpdate, Remote, SyncError,
};

use crate::wire::{
    API_PREFIX, PUSH_MEDIA_TYPE, WireError, WireFetch, WireOutcome, WireRef, WireUpdate,
    encode_push, error_from_wire,
};

/// The most a ref listing or a push's answer may take.
const MAX_ANSWER_BYTES: u64 = 16 << 20;

/// A store served at `base_url` + `/sync/v1/{store}`, reached with a bearer
/// token.
pub struct HttpRemote {
    base: String,
    store: String,
    token: Option<String>,
    agent: ureq::Agent,
    limits: BundleLimits,
}

impl HttpRemote {
    /// The store `store` at `base_url` (`https://hub.example.com`, say).
    /// Requests time out after five minutes.
    pub fn new(base_url: impl Into<String>, store: impl Into<String>) -> Self {
        let mut base = base_url.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self {
            base,
            store: store.into(),
            token: None,
            agent: agent(Some(Duration::from_secs(300))),
            limits: BundleLimits::default(),
        }
    }

    /// Send `token` as `Authorization: Bearer <token>`.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// How long a whole request may take; `None` for no limit.
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.agent = agent(timeout);
        self
    }

    /// Bound what a fetch may receive (the bundle reader checks it too).
    pub fn with_limits(mut self, limits: BundleLimits) -> Self {
        self.limits = limits;
        self
    }

    fn url(&self, route: &str) -> Result<String, SyncError> {
        let simple = |c: char| c.is_ascii_alphanumeric() || "-_.".contains(c);
        if self.store.is_empty() || !self.store.chars().all(simple) {
            return Err(SyncError::Remote(format!(
                "not a store name: {:?}",
                self.store
            )));
        }
        Ok(format!("{}{API_PREFIX}/{}/{route}", self.base, self.store))
    }

    fn authorized<B>(&self, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.token {
            Some(t) => request.header("Authorization", format!("Bearer {t}")),
            None => request,
        }
    }
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(timeout)
        .user_agent(concat!(
            "agentstategraph-sync-http/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .into()
}

fn transport(e: ureq::Error) -> SyncError {
    SyncError::Remote(e.to_string())
}

/// The answer's body, or the error it stands for.
fn check(response: ureq::http::Response<ureq::Body>) -> Result<ureq::Body, SyncError> {
    let status = response.status().as_u16();
    let mut body = response.into_body();
    if status == 200 {
        return Ok(body);
    }
    let text = body
        .with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_string()
        .unwrap_or_default();
    Err(match serde_json::from_str::<WireError>(&text) {
        Ok(error) => error_from_wire(status, error),
        Err(_) => SyncError::Remote(format!("{status}: {}", text.trim())),
    })
}

fn read_json<T: serde::de::DeserializeOwned>(body: ureq::Body) -> Result<T, SyncError> {
    let bytes = body
        .into_with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_vec()
        .map_err(transport)?;
    serde_json::from_slice(&bytes).map_err(|e| SyncError::Remote(format!("an answer: {e}")))
}

impl Remote for HttpRemote {
    fn list_refs(&self) -> Result<Vec<BundleRef>, SyncError> {
        let response = self
            .authorized(self.agent.get(self.url("refs")?))
            .call()
            .map_err(transport)?;
        let refs: Vec<WireRef> = read_json(check(response)?)?;
        refs.into_iter()
            .map(|r| r.into_ref().map_err(SyncError::Remote))
            .collect()
    }

    fn fetch(&self, request: &FetchRequest) -> Result<Box<dyn Read + Send + '_>, SyncError> {
        let json = serde_json::to_vec(&WireFetch::from(request))
            .map_err(|e| SyncError::Remote(e.to_string()))?;
        let response = self
            .authorized(self.agent.post(self.url("fetch")?))
            .content_type("application/json")
            .send(&json[..])
            .map_err(transport)?;
        let body = check(response)?;
        Ok(Box::new(
            body.into_with_config()
                .limit(self.limits.max_bytes)
                .reader(),
        ))
    }

    fn push(&self, update: &RefUpdate, bundle: &[u8]) -> Result<PushOutcome, SyncError> {
        let body = encode_push(&WireUpdate::from(update), bundle);
        let response = self
            .authorized(self.agent.post(self.url("push")?))
            .content_type(PUSH_MEDIA_TYPE)
            .send(&body[..])
            .map_err(transport)?;
        let outcome: WireOutcome = read_json(check(response)?)?;
        outcome.into_outcome().map_err(SyncError::Remote)
    }
}
