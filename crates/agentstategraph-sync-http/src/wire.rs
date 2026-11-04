//! What goes over the wire: JSON for refs, requests and answers, bundles as
//! they are ([`agentstategraph::sync::bundle`]), and a push as its update
//! and bundle in one body.
//!
//! The API's version is in its path, `/sync/v1`; a bundle's format version
//! is in its header, which the reader checks.

use agentstategraph::sync::{
    BundleRef, FetchRequest, PushOutcome, PushOverride, RefUpdate, SyncError,
};
use agentstategraph_core::{Namespace, ObjectId};
use serde::{Deserialize, Serialize};

/// Where the API lives, below a host's base URL.
pub const API_PREFIX: &str = "/sync/v1";

/// A fetch's answer.
pub const BUNDLE_MEDIA_TYPE: &str = "application/vnd.agentstategraph.bundle";

/// A push's body: [`encode_push`].
pub const PUSH_MEDIA_TYPE: &str = "application/vnd.agentstategraph.push";

/// The most a push's update may take, ahead of its bundle.
const MAX_UPDATE_BYTES: usize = 1 << 20;

/// A ref a store offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRef {
    pub namespace: String,
    pub name: String,
    pub target: String,
}

/// A ref by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRefName {
    pub namespace: String,
    pub name: String,
}

/// A fetch's request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFetch {
    pub refs: Vec<WireRefName>,
    pub haves: Vec<String>,
}

/// A push's update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireUpdate {
    pub namespace: String,
    pub name: String,
    pub old: Option<String>,
    pub new: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<PushOverride>,
}

/// A push's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WireOutcome {
    Updated,
    Stale { current: Option<String> },
    NotFastForward,
}

/// An error's body: a stable code and a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

fn id(hex: &str) -> Result<ObjectId, String> {
    ObjectId::from_hex(hex).ok_or_else(|| format!("not a commit id: {hex:?}"))
}

fn namespace(name: &str) -> Result<Namespace, String> {
    Namespace::new(name).map_err(|e| format!("not a namespace: {name:?}: {e}"))
}

impl From<&BundleRef> for WireRef {
    fn from(r: &BundleRef) -> Self {
        Self {
            namespace: r.namespace.to_string(),
            name: r.name.clone(),
            target: r.target.to_hex(),
        }
    }
}

impl WireRef {
    pub fn into_ref(self) -> Result<BundleRef, String> {
        Ok(BundleRef {
            namespace: namespace(&self.namespace)?,
            target: id(&self.target)?,
            name: self.name,
        })
    }
}

impl From<&FetchRequest> for WireFetch {
    fn from(r: &FetchRequest) -> Self {
        Self {
            refs: r
                .refs
                .iter()
                .map(|(ns, name)| WireRefName {
                    namespace: ns.to_string(),
                    name: name.clone(),
                })
                .collect(),
            haves: r.haves.iter().map(|h| h.to_hex()).collect(),
        }
    }
}

impl WireFetch {
    pub fn into_request(self) -> Result<FetchRequest, String> {
        Ok(FetchRequest {
            refs: self
                .refs
                .into_iter()
                .map(|r| Ok((namespace(&r.namespace)?, r.name)))
                .collect::<Result<_, String>>()?,
            haves: self
                .haves
                .iter()
                .map(|h| id(h))
                .collect::<Result<_, String>>()?,
        })
    }
}

impl From<&RefUpdate> for WireUpdate {
    fn from(u: &RefUpdate) -> Self {
        Self {
            namespace: u.namespace.to_string(),
            name: u.name.clone(),
            old: u.old.map(|o| o.to_hex()),
            new: u.new.to_hex(),
            overrides: u.overrides.clone(),
        }
    }
}

impl WireUpdate {
    pub fn into_update(self) -> Result<RefUpdate, String> {
        Ok(RefUpdate {
            namespace: namespace(&self.namespace)?,
            old: self.old.as_deref().map(id).transpose()?,
            new: id(&self.new)?,
            name: self.name,
            overrides: self.overrides,
        })
    }
}

impl From<&PushOutcome> for WireOutcome {
    fn from(o: &PushOutcome) -> Self {
        match o {
            PushOutcome::Updated => WireOutcome::Updated,
            PushOutcome::Stale { current } => WireOutcome::Stale {
                current: current.map(|c| c.to_hex()),
            },
            PushOutcome::NotFastForward => WireOutcome::NotFastForward,
        }
    }
}

impl WireOutcome {
    pub fn into_outcome(self) -> Result<PushOutcome, String> {
        Ok(match self {
            WireOutcome::Updated => PushOutcome::Updated,
            WireOutcome::Stale { current } => PushOutcome::Stale {
                current: current.as_deref().map(id).transpose()?,
            },
            WireOutcome::NotFastForward => PushOutcome::NotFastForward,
        })
    }
}

/// A push's body: the update's JSON, length first (four bytes, big-endian),
/// then the bundle.
pub fn encode_push(update: &WireUpdate, bundle: &[u8]) -> Vec<u8> {
    let json = serde_json::to_vec(update).expect("an update serializes");
    let mut body = Vec::with_capacity(4 + json.len() + bundle.len());
    body.extend_from_slice(&(json.len() as u32).to_be_bytes());
    body.extend_from_slice(&json);
    body.extend_from_slice(bundle);
    body
}

/// Split a push's body into its update and bundle.
pub fn decode_push(body: &[u8]) -> Result<(WireUpdate, &[u8]), String> {
    let (len, rest) = body
        .split_first_chunk::<4>()
        .ok_or("a push's body starts with its update's length")?;
    let len = u32::from_be_bytes(*len) as usize;
    if len > MAX_UPDATE_BYTES || len > rest.len() {
        return Err(format!("a push's update can't be {len} bytes"));
    }
    let (json, bundle) = rest.split_at(len);
    let update = serde_json::from_slice(json).map_err(|e| format!("a push's update: {e}"))?;
    Ok((update, bundle))
}

/// The HTTP status a receiving side answers `e` with.
pub fn status_for(e: &SyncError) -> u16 {
    use agentstategraph::RepoError;
    use agentstategraph::sync::TransferError;
    match e {
        SyncError::NoSuchRef(_) => 404,
        SyncError::Refused(_) => 403,
        SyncError::Transfer(TransferError::Bundle(_) | TransferError::Incomplete(_)) => 400,
        SyncError::Transfer(TransferError::Repo(RepoError::ReservedRef(_))) => 403,
        SyncError::Transfer(TransferError::Repo(RepoError::InvalidOperation(_))) => 400,
        _ => 500,
    }
}

/// The error a receiving side's answer stands for, from its status and
/// body. The codes a receiver gives back become the same errors here;
/// anything else is [`SyncError::Remote`].
pub fn error_from_wire(status: u16, error: WireError) -> SyncError {
    match error.code.as_str() {
        "no_such_ref" => SyncError::NoSuchRef(error.message),
        "push_refused" => SyncError::Refused(error.message),
        _ => SyncError::Remote(format!("{status} {}: {}", error.code, error.message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_round_trips() {
        let update = WireUpdate {
            namespace: "default".into(),
            name: "main".into(),
            old: None,
            new: ObjectId::hash(b"x").to_hex(),
            overrides: vec![PushOverride {
                finding: "f".into(),
                reason: "r".into(),
            }],
        };
        let body = encode_push(&update, b"bundle");
        let (back, bundle) = decode_push(&body).unwrap();
        assert_eq!(back, update);
        assert_eq!(bundle, b"bundle");
        assert_eq!(back.into_update().unwrap().new, ObjectId::hash(b"x"));
    }

    #[test]
    fn a_malformed_push_is_refused() {
        assert!(decode_push(b"").is_err());
        assert!(decode_push(&[0, 0, 0, 9, b'{']).is_err());
        assert!(decode_push(&[0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn outcomes_round_trip() {
        for o in [
            PushOutcome::Updated,
            PushOutcome::Stale { current: None },
            PushOutcome::Stale {
                current: Some(ObjectId::hash(b"c")),
            },
            PushOutcome::NotFastForward,
        ] {
            let json = serde_json::to_string(&WireOutcome::from(&o)).unwrap();
            let back: WireOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(back.into_outcome().unwrap(), o);
        }
    }
}
