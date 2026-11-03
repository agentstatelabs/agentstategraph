//! Bundles (DESIGN.md Plan G t-002): commits, objects and refs in one
//! compressed blob, for sync and for store export and import.
//!
//! A reader never trusts a bundle's ids. It recomputes the id of every commit
//! and object from its content, and the first mismatch fails the read. It
//! hands out records one at a time, each verified, so a caller can apply a
//! large bundle without holding it in memory; one that stores commits and
//! objects as they come and moves refs only once the reader reaches the end
//! leaves nothing reachable from a bundle that failed. [`Bundle::from_bytes`]
//! reads the whole bundle before it returns anything.
//!
//! A bundle needn't be complete. A commit's parents and objects may already
//! be on the receiving side, and a shallow record marks a commit whose
//! parents are left out on purpose. Checking that what a bundle refers to is
//! there is the importer's job.
//!
//! # Format, version 1
//!
//! ```text
//! header   "ASGB"  u16 format version  u8 compression  u8 reserved (0)
//! body     the records, compressed with zstd (compression 1)
//! record   u8 kind  u32 payload length  payload
//! ```
//!
//! Integers are big-endian. Records come in any order:
//!
//! | kind | record  | payload |
//! |------|---------|---------|
//! | 1    | ref     | u16 length + namespace, u16 length + ref name (UTF-8), 32-byte target commit |
//! | 2    | shallow | 32-byte commit id: the commit's parents are left out on purpose |
//! | 3    | commit  | the commit as JSON, as storage keeps it, its `id` included |
//! | 4    | object  | 32-byte id, then the object's canonical JSON |
//! | 0    | end     | empty; the last record |
//!
//! A bundle without its end record is truncated, and nothing may follow it.
//! An unknown record kind fails the read, since a reader that skipped one
//! couldn't know what it had missed; a new kind comes with a new format
//! version. A repeated commit or object is harmless (same id, same content)
//! and counts toward the limits; a repeated ref or shallow record is
//! malformed.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agentstategraph_core::{Commit, Namespace, Object, ObjectId};

/// The first four bytes of every bundle.
pub const MAGIC: [u8; 4] = *b"ASGB";

/// The format version this build writes, and the only one it reads.
pub const FORMAT_VERSION: u16 = 1;

const COMPRESSION_ZSTD: u8 = 1;
const ZSTD_LEVEL: i32 = 3;
const HEADER_LEN: usize = 8;

const KIND_END: u8 = 0;
const KIND_REF: u8 = 1;
const KIND_SHALLOW: u8 = 2;
const KIND_COMMIT: u8 = 3;
const KIND_OBJECT: u8 = 4;

/// A ref a bundle carries: where `name` in `namespace` points.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BundleRef {
    pub namespace: Namespace,
    pub name: String,
    pub target: ObjectId,
}

/// One record of a bundle, as [`BundleReader`] returns it: already verified.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Ref(BundleRef),
    /// A commit whose parents the bundle leaves out on purpose.
    Shallow(ObjectId),
    /// A commit whose `id` matches its content.
    Commit(Box<Commit>),
    /// An object and its id, which matches its content.
    Object(ObjectId, Object),
}

/// How much of a bundle a reader accepts before it gives up. The defaults
/// suit a push or fetch; an import of a whole store from a local file may
/// want [`BundleLimits::unlimited`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BundleLimits {
    /// Bytes of the bundle as received, header included.
    pub max_bytes: u64,
    /// Bytes of its records once decompressed, which bounds a bundle that
    /// compresses far better than real data does.
    pub max_uncompressed_bytes: u64,
    /// Bytes of any one record's payload.
    pub max_record_bytes: u32,
    pub max_refs: u64,
    pub max_commits: u64,
    pub max_objects: u64,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 << 20,
            max_uncompressed_bytes: 1 << 30,
            max_record_bytes: 64 << 20,
            max_refs: 10_000,
            max_commits: 1_000_000,
            max_objects: 10_000_000,
        }
    }
}

impl BundleLimits {
    /// No limits beyond the format's own (a record's payload is at most
    /// `u32::MAX` bytes). For a bundle from a trusted source.
    pub fn unlimited() -> Self {
        Self {
            max_bytes: u64::MAX,
            max_uncompressed_bytes: u64::MAX,
            max_record_bytes: u32::MAX,
            max_refs: u64::MAX,
            max_commits: u64::MAX,
            max_objects: u64::MAX,
        }
    }
}

/// Why a bundle couldn't be written or read.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("not an AgentStateGraph bundle")]
    NotABundle,

    #[error("bundle format version {0} isn't supported; this build reads version {FORMAT_VERSION}")]
    UnsupportedVersion(u16),

    #[error("bundle compression {0} isn't supported")]
    UnsupportedCompression(u8),

    #[error("bundle is over its limit of {limit} {what}")]
    OverLimit { what: &'static str, limit: u64 },

    #[error("bundle ends early")]
    Truncated,

    #[error("malformed bundle: {0}")]
    Malformed(String),

    /// A commit or object whose content doesn't hash to the id it came with.
    #[error("bundle {kind} {claimed} doesn't match its content, which hashes to {actual}")]
    IdMismatch {
        kind: &'static str,
        claimed: ObjectId,
        actual: ObjectId,
    },

    #[error("bundle repeats {kind} {what}")]
    Duplicate { kind: &'static str, what: String },

    #[error("bundle I/O: {0}")]
    Io(#[from] io::Error),
}

impl BundleError {
    /// A stable code for this kind of error, as `StorageError::code` gives.
    pub fn code(&self) -> &'static str {
        match self {
            BundleError::NotABundle => "not_a_bundle",
            BundleError::UnsupportedVersion(_) => "bundle_version_unsupported",
            BundleError::UnsupportedCompression(_) => "bundle_compression_unsupported",
            BundleError::OverLimit { .. } => "bundle_too_large",
            BundleError::Truncated => "bundle_truncated",
            BundleError::Malformed(_) => "bundle_malformed",
            BundleError::IdMismatch { .. } => "bundle_id_mismatch",
            BundleError::Duplicate { .. } => "bundle_duplicate",
            BundleError::Io(_) => "bundle_io",
        }
    }
}

/// Writes a bundle to `W` a record at a time. [`finish`](Self::finish) writes
/// the end record; a bundle dropped before it reads as truncated.
///
/// The writer trusts what it's given: it writes each commit's own `id`, and
/// an object under the id of its content. A repeated commit or object is
/// written once; a repeated ref fails.
pub struct BundleWriter<W: Write> {
    encoder: zstd::stream::write::Encoder<'static, W>,
    refs: HashSet<(Namespace, String)>,
    shallow: HashSet<ObjectId>,
}

impl<W: Write> BundleWriter<W> {
    /// Write the header and start the records.
    pub fn new(mut out: W) -> Result<Self, BundleError> {
        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(&MAGIC);
        header[4..6].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
        header[6] = COMPRESSION_ZSTD;
        out.write_all(&header)?;
        let mut encoder = zstd::stream::write::Encoder::new(out, ZSTD_LEVEL)?;
        // Lets the reader tell a stream cut short from one that ended.
        encoder.include_checksum(true)?;
        Ok(Self {
            encoder,
            refs: HashSet::new(),
            shallow: HashSet::new(),
        })
    }

    pub fn write_ref(&mut self, r: &BundleRef) -> Result<(), BundleError> {
        if r.name.is_empty() {
            return Err(BundleError::Malformed("a ref needs a name".into()));
        }
        if !self.refs.insert((r.namespace.clone(), r.name.clone())) {
            return Err(BundleError::Duplicate {
                kind: "ref",
                what: format!("{}/{}", r.namespace, r.name),
            });
        }
        let mut payload = Vec::with_capacity(4 + r.namespace.as_str().len() + r.name.len() + 32);
        put_str(&mut payload, r.namespace.as_str())?;
        put_str(&mut payload, &r.name)?;
        payload.extend_from_slice(r.target.as_bytes());
        self.record(KIND_REF, &payload)
    }

    /// Mark `commit` as one whose parents the bundle leaves out on purpose.
    pub fn write_shallow(&mut self, commit: &ObjectId) -> Result<(), BundleError> {
        if !self.shallow.insert(*commit) {
            return Ok(());
        }
        self.record(KIND_SHALLOW, commit.as_bytes())
    }

    pub fn write_commit(&mut self, commit: &Commit) -> Result<(), BundleError> {
        let json = serde_json::to_vec(commit)
            .map_err(|e| BundleError::Malformed(format!("commit {}: {e}", commit.id)))?;
        self.record(KIND_COMMIT, &json)
    }

    pub fn write_object(&mut self, object: &Object) -> Result<(), BundleError> {
        let canonical = object.canonical_bytes();
        let id = ObjectId::hash(&canonical);
        let mut payload = Vec::with_capacity(32 + canonical.len());
        payload.extend_from_slice(id.as_bytes());
        payload.extend_from_slice(&canonical);
        self.record(KIND_OBJECT, &payload)
    }

    /// Write the end record and finish the compressed stream, returning `W`.
    pub fn finish(mut self) -> Result<W, BundleError> {
        self.record(KIND_END, &[])?;
        Ok(self.encoder.finish()?)
    }

    fn record(&mut self, kind: u8, payload: &[u8]) -> Result<(), BundleError> {
        let len = u32::try_from(payload.len()).map_err(|_| BundleError::OverLimit {
            what: "bytes in one record",
            limit: u64::from(u32::MAX),
        })?;
        self.encoder.write_all(&[kind])?;
        self.encoder.write_all(&len.to_be_bytes())?;
        self.encoder.write_all(payload)?;
        Ok(())
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) -> Result<(), BundleError> {
    let len = u16::try_from(s.len())
        .map_err(|_| BundleError::Malformed(format!("name over {} bytes", u16::MAX)))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Passes reads through until `remaining` bytes have gone by, then fails
/// them, setting `over` so the reader can tell that from other I/O errors.
struct Capped<R> {
    inner: R,
    remaining: u64,
    over: Arc<AtomicBool>,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            // At the cap: fine if the input is done too, a failure if not.
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => {
                    self.over.store(true, Ordering::Relaxed);
                    Err(io::Error::other("bundle over its byte limit"))
                }
            };
        }
        let want = buf
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Reads a bundle from `R` a record at a time, verifying each one: see the
/// [module docs](self). Also an [`Iterator`] over the records, which stops
/// after the first error.
pub struct BundleReader<R: Read> {
    decoder: zstd::stream::read::Decoder<'static, io::BufReader<Capped<R>>>,
    over_cap: Arc<AtomicBool>,
    limits: BundleLimits,
    uncompressed: u64,
    refs: HashSet<(Namespace, String)>,
    shallow: HashSet<ObjectId>,
    commits: u64,
    objects: u64,
    done: bool,
}

impl<R: Read> BundleReader<R> {
    /// Read and check the header.
    pub fn new(input: R, limits: BundleLimits) -> Result<Self, BundleError> {
        let over_cap = Arc::new(AtomicBool::new(false));
        let mut capped = Capped {
            inner: input,
            remaining: limits.max_bytes,
            over: over_cap.clone(),
        };
        let mut header = [0u8; HEADER_LEN];
        if let Err(e) = capped.read_exact(&mut header) {
            return Err(match e.kind() {
                io::ErrorKind::UnexpectedEof => BundleError::NotABundle,
                _ if over_cap.load(Ordering::Relaxed) => BundleError::OverLimit {
                    what: "bytes",
                    limit: limits.max_bytes,
                },
                _ => BundleError::Io(e),
            });
        }
        if header[..4] != MAGIC {
            return Err(BundleError::NotABundle);
        }
        let version = u16::from_be_bytes([header[4], header[5]]);
        if version != FORMAT_VERSION {
            return Err(BundleError::UnsupportedVersion(version));
        }
        if header[6] != COMPRESSION_ZSTD {
            return Err(BundleError::UnsupportedCompression(header[6]));
        }
        if header[7] != 0 {
            return Err(BundleError::Malformed(
                "reserved header byte isn't 0".into(),
            ));
        }
        Ok(Self {
            decoder: zstd::stream::read::Decoder::new(capped)?,
            over_cap,
            limits,
            uncompressed: 0,
            refs: HashSet::new(),
            shallow: HashSet::new(),
            commits: 0,
            objects: 0,
            done: false,
        })
    }

    /// The next record, verified, or `None` once the end record has been
    /// read and nothing follows it.
    pub fn next_record(&mut self) -> Result<Option<Record>, BundleError> {
        if self.done {
            return Ok(None);
        }
        let result = self.read_record();
        if !matches!(result, Ok(Some(_))) {
            self.done = true;
        }
        result
    }

    /// Read and verify every remaining record, discarding them: `Ok` only if
    /// the whole bundle is intact.
    pub fn verify_rest(&mut self) -> Result<(), BundleError> {
        while self.next_record()?.is_some() {}
        Ok(())
    }

    fn read_record(&mut self) -> Result<Option<Record>, BundleError> {
        let mut head = [0u8; 5];
        self.read_body(&mut head)?;
        let kind = head[0];
        let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
        if len > self.limits.max_record_bytes {
            return Err(BundleError::OverLimit {
                what: "bytes in one record",
                limit: u64::from(self.limits.max_record_bytes),
            });
        }
        self.uncompressed = self.uncompressed.saturating_add(5 + u64::from(len));
        if self.uncompressed > self.limits.max_uncompressed_bytes {
            return Err(BundleError::OverLimit {
                what: "uncompressed bytes",
                limit: self.limits.max_uncompressed_bytes,
            });
        }
        let payload = self.read_payload(len)?;
        match kind {
            KIND_END => {
                if !payload.is_empty() {
                    return Err(BundleError::Malformed(
                        "the end record has a payload".into(),
                    ));
                }
                let mut extra = [0u8; 1];
                let read = self.decoder.read(&mut extra);
                if read.map_err(|e| self.io_error(e))? != 0 {
                    return Err(BundleError::Malformed(
                        "records follow the end record".into(),
                    ));
                }
                Ok(None)
            }
            KIND_REF => {
                let r = parse_ref(&payload)?;
                if self.refs.len() as u64 >= self.limits.max_refs {
                    return Err(BundleError::OverLimit {
                        what: "refs",
                        limit: self.limits.max_refs,
                    });
                }
                if !self.refs.insert((r.namespace.clone(), r.name.clone())) {
                    return Err(BundleError::Duplicate {
                        kind: "ref",
                        what: format!("{}/{}", r.namespace, r.name),
                    });
                }
                Ok(Some(Record::Ref(r)))
            }
            KIND_SHALLOW => {
                let id = parse_id(&payload, "shallow record")?;
                if self.shallow.len() as u64 >= self.limits.max_commits {
                    return Err(BundleError::OverLimit {
                        what: "shallow commits",
                        limit: self.limits.max_commits,
                    });
                }
                if !self.shallow.insert(id) {
                    return Err(BundleError::Duplicate {
                        kind: "shallow commit",
                        what: id.to_string(),
                    });
                }
                Ok(Some(Record::Shallow(id)))
            }
            KIND_COMMIT => {
                if self.commits >= self.limits.max_commits {
                    return Err(BundleError::OverLimit {
                        what: "commits",
                        limit: self.limits.max_commits,
                    });
                }
                self.commits += 1;
                let commit: Commit = serde_json::from_slice(&payload)
                    .map_err(|e| BundleError::Malformed(format!("a commit record: {e}")))?;
                let actual = commit.compute_id();
                if actual != commit.id {
                    return Err(BundleError::IdMismatch {
                        kind: "commit",
                        claimed: commit.id,
                        actual,
                    });
                }
                Ok(Some(Record::Commit(Box::new(commit))))
            }
            KIND_OBJECT => {
                if self.objects >= self.limits.max_objects {
                    return Err(BundleError::OverLimit {
                        what: "objects",
                        limit: self.limits.max_objects,
                    });
                }
                self.objects += 1;
                if payload.len() < 32 {
                    return Err(BundleError::Malformed(
                        "an object record is shorter than its id".into(),
                    ));
                }
                let claimed = parse_id(&payload[..32], "object record")?;
                let object: Object = serde_json::from_slice(&payload[32..])
                    .map_err(|e| BundleError::Malformed(format!("object {claimed}: {e}")))?;
                // From the object as parsed, so an id can only name the
                // canonical form of what's stored under it.
                let actual = object.id();
                if actual != claimed {
                    return Err(BundleError::IdMismatch {
                        kind: "object",
                        claimed,
                        actual,
                    });
                }
                Ok(Some(Record::Object(claimed, object)))
            }
            other => Err(BundleError::Malformed(format!(
                "unknown record kind {other}"
            ))),
        }
    }

    fn read_body(&mut self, buf: &mut [u8]) -> Result<(), BundleError> {
        let read = self.decoder.read_exact(buf);
        read.map_err(|e| self.io_error(e))
    }

    fn read_payload(&mut self, len: u32) -> Result<Vec<u8>, BundleError> {
        // Grown as the data arrives, so a record that claims more than it
        // has can't make the reader allocate its claim.
        let mut payload = Vec::new();
        let read = (&mut self.decoder)
            .take(u64::from(len))
            .read_to_end(&mut payload);
        read.map_err(|e| self.io_error(e))?;
        if payload.len() != len as usize {
            return Err(BundleError::Truncated);
        }
        Ok(payload)
    }

    fn io_error(&self, e: io::Error) -> BundleError {
        if self.over_cap.load(Ordering::Relaxed) {
            BundleError::OverLimit {
                what: "bytes",
                limit: self.limits.max_bytes,
            }
        } else if e.kind() == io::ErrorKind::UnexpectedEof {
            BundleError::Truncated
        } else {
            BundleError::Io(e)
        }
    }
}

impl<R: Read> Iterator for BundleReader<R> {
    type Item = Result<Record, BundleError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_record().transpose()
    }
}

fn parse_id(bytes: &[u8], what: &str) -> Result<ObjectId, BundleError> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| BundleError::Malformed(format!("a {what} needs a 32-byte id")))?;
    Ok(ObjectId::from_bytes(bytes))
}

fn parse_ref(payload: &[u8]) -> Result<BundleRef, BundleError> {
    let mut rest = payload;
    let namespace = take_str(&mut rest)?;
    let name = take_str(&mut rest)?;
    let target = parse_id(rest, "ref record")?;
    let namespace = Namespace::new(namespace)
        .map_err(|e| BundleError::Malformed(format!("a ref's namespace: {e}")))?;
    if name.is_empty() {
        return Err(BundleError::Malformed("a ref needs a name".into()));
    }
    Ok(BundleRef {
        namespace,
        name,
        target,
    })
}

fn take_str(rest: &mut &[u8]) -> Result<String, BundleError> {
    let short = || BundleError::Malformed("a ref record ends early".into());
    let (len, tail) = rest.split_first_chunk::<2>().ok_or_else(short)?;
    let len = usize::from(u16::from_be_bytes(*len));
    if tail.len() < len {
        return Err(short());
    }
    let (s, tail) = tail.split_at(len);
    *rest = tail;
    String::from_utf8(s.to_vec())
        .map_err(|_| BundleError::Malformed("a ref name isn't UTF-8".into()))
}

/// A whole bundle in memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bundle {
    pub refs: Vec<BundleRef>,
    pub shallow: Vec<ObjectId>,
    pub commits: Vec<Commit>,
    pub objects: Vec<Object>,
}

impl Bundle {
    /// Encode the bundle: objects, commits, shallow records, then refs, so
    /// a reader meets what each ref points at before the ref itself.
    pub fn to_bytes(&self) -> Result<Vec<u8>, BundleError> {
        let mut writer = BundleWriter::new(Vec::new())?;
        for object in &self.objects {
            writer.write_object(object)?;
        }
        for commit in &self.commits {
            writer.write_commit(commit)?;
        }
        for id in &self.shallow {
            writer.write_shallow(id)?;
        }
        for r in &self.refs {
            writer.write_ref(r)?;
        }
        writer.finish()
    }

    /// Decode and verify a whole bundle. Fails, returning nothing, on the
    /// first record that doesn't verify.
    pub fn from_bytes(bytes: &[u8], limits: &BundleLimits) -> Result<Self, BundleError> {
        let mut reader = BundleReader::new(bytes, limits.clone())?;
        let mut bundle = Bundle::default();
        while let Some(record) = reader.next_record()? {
            match record {
                Record::Ref(r) => bundle.refs.push(r),
                Record::Shallow(id) => bundle.shallow.push(id),
                Record::Commit(c) => bundle.commits.push(*c),
                Record::Object(_, o) => bundle.objects.push(o),
            }
        }
        Ok(bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentstategraph_core::{Authority, CommitBuilder, Intent, IntentCategory, ToolCall};
    use std::collections::BTreeMap;

    fn objects() -> Vec<Object> {
        let a = Object::string("alpha");
        let b = Object::int(-7);
        let mut entries = BTreeMap::new();
        entries.insert("a".to_string(), a.id());
        entries.insert("b".to_string(), b.id());
        vec![
            a.clone(),
            b.clone(),
            Object::map(entries),
            Object::Node(agentstategraph_core::Node::List(vec![a.id(), b.id()])),
            Object::bytes(vec![0, 1, 2, 255]),
            Object::float(1.5),
            Object::uint(u64::MAX),
            Object::bool(true),
            Object::null(),
        ]
    }

    fn commit(root: ObjectId, parents: Vec<ObjectId>, what: &str) -> Commit {
        CommitBuilder::new(
            root,
            "agent/bundle-test",
            Authority::simple("tester"),
            Intent::new(IntentCategory::Refine, what),
        )
        .parents(parents)
        .reasoning("because")
        .confidence(0.25)
        .tool_calls(vec![ToolCall {
            tool_name: "set".into(),
            arguments: serde_json::json!({"z": 1, "a": [true, null, 2.5]}),
            result: Some("ok".into()),
            timestamp: chrono::Utc::now(),
        }])
        .build()
    }

    fn sample() -> Bundle {
        let objects = objects();
        let base = commit(objects[2].id(), vec![], "base");
        let left = commit(objects[0].id(), vec![base.id], "left");
        let right = commit(objects[1].id(), vec![base.id], "right");
        let merge = commit(objects[3].id(), vec![left.id, right.id], "merge");
        Bundle {
            refs: vec![
                BundleRef {
                    namespace: Namespace::default_ns(),
                    name: "main".into(),
                    target: merge.id,
                },
                BundleRef {
                    namespace: Namespace::new("team-a").unwrap(),
                    name: "remotes/hub/main".into(),
                    target: left.id,
                },
            ],
            shallow: vec![base.id],
            commits: vec![base, left, right, merge],
            objects,
        }
    }

    /// A bundle body built from raw records, with the header given.
    fn raw(header: [u8; HEADER_LEN], records: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (kind, payload) in records {
            body.push(*kind);
            body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            body.extend_from_slice(payload);
        }
        let mut out = header.to_vec();
        out.extend(zstd::encode_all(&body[..], 1).unwrap());
        out
    }

    fn header() -> [u8; HEADER_LEN] {
        let mut h = [0u8; HEADER_LEN];
        h[..4].copy_from_slice(&MAGIC);
        h[4..6].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
        h[6] = COMPRESSION_ZSTD;
        h
    }

    fn object_record(claimed: ObjectId, object: &Object) -> (u8, Vec<u8>) {
        let mut payload = claimed.as_bytes().to_vec();
        payload.extend(object.canonical_bytes());
        (KIND_OBJECT, payload)
    }

    fn end() -> (u8, Vec<u8>) {
        (KIND_END, vec![])
    }

    fn read(bytes: &[u8]) -> Result<Bundle, BundleError> {
        Bundle::from_bytes(bytes, &BundleLimits::default())
    }

    #[test]
    fn a_bundle_round_trips() {
        let bundle = sample();
        let bytes = bundle.to_bytes().unwrap();
        assert_eq!(&bytes[..4], b"ASGB");
        assert_eq!(read(&bytes).unwrap(), bundle);
    }

    #[test]
    fn an_empty_bundle_round_trips() {
        let bytes = Bundle::default().to_bytes().unwrap();
        assert_eq!(read(&bytes).unwrap(), Bundle::default());
    }

    #[test]
    fn the_reader_returns_records_in_order_then_none() {
        let bundle = sample();
        let bytes = bundle.to_bytes().unwrap();
        let mut reader = BundleReader::new(&bytes[..], BundleLimits::default()).unwrap();
        let records: Vec<Record> = reader.by_ref().collect::<Result<_, _>>().unwrap();
        assert_eq!(
            records.len(),
            bundle.refs.len() + bundle.shallow.len() + bundle.commits.len() + bundle.objects.len()
        );
        assert_eq!(
            records[0],
            Record::Object(bundle.objects[0].id(), bundle.objects[0].clone())
        );
        assert_eq!(
            records.last(),
            Some(&Record::Ref(bundle.refs.last().unwrap().clone()))
        );
        assert!(reader.next_record().unwrap().is_none());
    }

    #[test]
    fn an_object_under_the_wrong_id_fails_the_whole_bundle() {
        let good = Object::string("good");
        let planted = Object::string("planted");
        let bytes = raw(
            header(),
            &[
                object_record(good.id(), &good),
                object_record(good.id(), &planted),
                end(),
            ],
        );
        match read(&bytes) {
            Err(BundleError::IdMismatch {
                kind,
                claimed,
                actual,
            }) => {
                assert_eq!(kind, "object");
                assert_eq!(claimed, good.id());
                assert_eq!(actual, planted.id());
            }
            other => panic!("expected IdMismatch, got {other:?}"),
        }
    }

    #[test]
    fn a_commit_edited_after_its_id_was_computed_fails() {
        let mut c = commit(ObjectId::hash(b"root"), vec![], "honest");
        c.intent.description = "edited".into();
        let bytes = raw(
            header(),
            &[(KIND_COMMIT, serde_json::to_vec(&c).unwrap()), end()],
        );
        match read(&bytes) {
            Err(e @ BundleError::IdMismatch { kind: "commit", .. }) => {
                assert_eq!(e.code(), "bundle_id_mismatch")
            }
            other => panic!("expected IdMismatch, got {other:?}"),
        }
    }

    #[test]
    fn a_non_canonical_object_is_checked_against_its_canonical_form() {
        // The same map, keys out of order: its bytes hash to something else,
        // but the object it parses to is the canonical one.
        let a = Object::string("a");
        let b = Object::string("b");
        let mut entries = BTreeMap::new();
        entries.insert("x".to_string(), a.id());
        entries.insert("y".to_string(), b.id());
        let map = Object::map(entries);
        let canonical = String::from_utf8(map.canonical_bytes()).unwrap();
        let x = format!("\"x\":{}", serde_json::to_string(&a.id()).unwrap());
        let y = format!("\"y\":{}", serde_json::to_string(&b.id()).unwrap());
        let reordered = canonical.replace(&format!("{x},{y}"), &format!("{y},{x}"));
        assert_ne!(reordered, canonical);

        let mut payload = map.id().as_bytes().to_vec();
        payload.extend(reordered.as_bytes());
        let bytes = raw(header(), &[(KIND_OBJECT, payload), end()]);
        assert_eq!(read(&bytes).unwrap().objects, vec![map.clone()]);

        // And an id that names the reordered bytes is refused.
        let mut payload = ObjectId::hash(reordered.as_bytes()).as_bytes().to_vec();
        payload.extend(reordered.as_bytes());
        let bytes = raw(header(), &[(KIND_OBJECT, payload), end()]);
        assert!(matches!(read(&bytes), Err(BundleError::IdMismatch { .. })));
    }

    #[test]
    fn a_bundle_without_its_end_is_truncated() {
        let o = Object::string("o");
        let bytes = raw(header(), &[object_record(o.id(), &o)]);
        assert!(matches!(read(&bytes), Err(BundleError::Truncated)));

        // Cut mid-stream: truncated, or the decompressor's own complaint.
        let full = sample().to_bytes().unwrap();
        for cut in [HEADER_LEN, HEADER_LEN + 3, full.len() / 2, full.len() - 1] {
            match read(&full[..cut]) {
                Err(BundleError::Truncated | BundleError::Io(_)) => {}
                other => panic!("cut at {cut}: expected a failure, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_record_claiming_more_than_it_has_is_truncated() {
        let mut body = vec![KIND_OBJECT];
        body.extend_from_slice(&1_000u32.to_be_bytes());
        body.extend_from_slice(&[0u8; 10]);
        let mut bytes = header().to_vec();
        bytes.extend(zstd::encode_all(&body[..], 1).unwrap());
        assert!(matches!(read(&bytes), Err(BundleError::Truncated)));
    }

    #[test]
    fn nothing_may_follow_the_end() {
        let bytes = raw(header(), &[end(), end()]);
        assert!(matches!(read(&bytes), Err(BundleError::Malformed(_))));

        let mut bytes = Bundle::default().to_bytes().unwrap();
        bytes.extend_from_slice(b"trailing garbage");
        assert!(read(&bytes).is_err());
    }

    #[test]
    fn an_unknown_record_kind_fails() {
        let bytes = raw(header(), &[(9, vec![1, 2, 3]), end()]);
        match read(&bytes) {
            Err(BundleError::Malformed(m)) => assert!(m.contains("kind 9"), "{m}"),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn the_header_is_checked() {
        assert!(matches!(read(b""), Err(BundleError::NotABundle)));
        assert!(matches!(read(b"ASG"), Err(BundleError::NotABundle)));
        assert!(matches!(
            read(b"PK\x03\x04\x00\x01\x01\x00"),
            Err(BundleError::NotABundle)
        ));

        let mut h = header();
        h[4..6].copy_from_slice(&2u16.to_be_bytes());
        assert!(matches!(
            read(&raw(h, &[end()])),
            Err(BundleError::UnsupportedVersion(2))
        ));

        let mut h = header();
        h[6] = 0;
        assert!(matches!(
            read(&raw(h, &[end()])),
            Err(BundleError::UnsupportedCompression(0))
        ));

        let mut h = header();
        h[7] = 1;
        assert!(matches!(
            read(&raw(h, &[end()])),
            Err(BundleError::Malformed(_))
        ));
    }

    #[test]
    fn each_limit_is_enforced() {
        let bundle = sample();
        let bytes = bundle.to_bytes().unwrap();
        let over = |limits: BundleLimits| match Bundle::from_bytes(&bytes, &limits) {
            Err(BundleError::OverLimit { what, .. }) => what,
            other => panic!("expected OverLimit, got {other:?}"),
        };

        assert_eq!(
            over(BundleLimits {
                max_objects: 3,
                ..BundleLimits::default()
            }),
            "objects"
        );
        assert_eq!(
            over(BundleLimits {
                max_commits: 2,
                ..BundleLimits::default()
            }),
            "commits"
        );
        assert_eq!(
            over(BundleLimits {
                max_refs: 1,
                ..BundleLimits::default()
            }),
            "refs"
        );
        assert_eq!(
            over(BundleLimits {
                max_record_bytes: 40,
                ..BundleLimits::default()
            }),
            "bytes in one record"
        );
        assert_eq!(
            over(BundleLimits {
                max_uncompressed_bytes: 100,
                ..BundleLimits::default()
            }),
            "uncompressed bytes"
        );
        assert_eq!(
            over(BundleLimits {
                max_bytes: 20,
                ..BundleLimits::default()
            }),
            "bytes"
        );

        // Exactly at the byte limit is fine.
        let exact = BundleLimits {
            max_bytes: bytes.len() as u64,
            ..BundleLimits::default()
        };
        assert_eq!(Bundle::from_bytes(&bytes, &exact).unwrap(), bundle);
    }

    #[test]
    fn a_bundle_that_decompresses_to_far_more_stops_at_the_limit() {
        // 16 MiB of one string compresses to a few KiB. The reader stops at
        // the record's declared length, before decompressing it.
        let huge = Object::string("x".repeat(16 << 20));
        let bytes = Bundle {
            objects: vec![huge],
            ..Bundle::default()
        }
        .to_bytes()
        .unwrap();
        assert!(bytes.len() < 1 << 20, "{} bytes", bytes.len());
        let limits = BundleLimits {
            max_uncompressed_bytes: 1 << 20,
            ..BundleLimits::default()
        };
        assert!(matches!(
            Bundle::from_bytes(&bytes, &limits),
            Err(BundleError::OverLimit {
                what: "uncompressed bytes",
                ..
            })
        ));
    }

    #[test]
    fn a_repeated_ref_is_refused_and_a_repeated_object_is_not() {
        let mut bundle = sample();
        bundle.refs.push(bundle.refs[0].clone());
        assert!(matches!(
            bundle.to_bytes(),
            Err(BundleError::Duplicate { kind: "ref", .. })
        ));

        let r = &sample().refs[0];
        let mut payload = Vec::new();
        put_str(&mut payload, r.namespace.as_str()).unwrap();
        put_str(&mut payload, &r.name).unwrap();
        payload.extend_from_slice(r.target.as_bytes());
        let bytes = raw(
            header(),
            &[(KIND_REF, payload.clone()), (KIND_REF, payload), end()],
        );
        assert!(matches!(
            read(&bytes),
            Err(BundleError::Duplicate { kind: "ref", .. })
        ));

        let o = Object::string("again");
        let bytes = raw(
            header(),
            &[object_record(o.id(), &o), object_record(o.id(), &o), end()],
        );
        assert_eq!(read(&bytes).unwrap().objects, vec![o.clone(), o]);
    }

    #[test]
    fn a_ref_with_a_bad_namespace_or_no_name_is_malformed() {
        let target = ObjectId::hash(b"t");
        for (ns, name) in [("bad ns!", "main"), ("default", "")] {
            let mut payload = Vec::new();
            put_str(&mut payload, ns).unwrap();
            put_str(&mut payload, name).unwrap();
            payload.extend_from_slice(target.as_bytes());
            let bytes = raw(header(), &[(KIND_REF, payload), end()]);
            assert!(
                matches!(read(&bytes), Err(BundleError::Malformed(_))),
                "{ns:?} {name:?}"
            );
        }
    }

    #[test]
    fn the_iterator_stops_after_an_error() {
        let good = Object::string("good");
        let bytes = raw(
            header(),
            &[
                object_record(good.id(), &good),
                object_record(ObjectId::hash(b"wrong"), &good),
                object_record(good.id(), &good),
                end(),
            ],
        );
        let mut reader = BundleReader::new(&bytes[..], BundleLimits::default()).unwrap();
        assert!(matches!(reader.next(), Some(Ok(Record::Object(..)))));
        assert!(matches!(
            reader.next(),
            Some(Err(BundleError::IdMismatch { .. }))
        ));
        assert!(reader.next().is_none());
    }
}
