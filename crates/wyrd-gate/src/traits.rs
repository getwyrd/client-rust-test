//! The `MetadataStore` contract, **vendored verbatim** from the wyrd repo
//! (`crates/traits/src/lib.rs` @ 7009c2e, 2026-07-01) so this harness stays
//! standalone — it depends only on the sibling `client-rust` checkout.
//!
//! Copied, not depended on, by design: the gate must exercise the *exact*
//! seam `metadata-tikv` will implement, and M4's whole premise is that this
//! trait is frozen ("byte-for-byte unchanged", proposal 0015). If wyrd's
//! `wyrd-traits` ever changes this surface, re-copy and note the delta —
//! a drift here would itself be a gate finding.
//!
//! Only the metadata seam is copied (`BoxError`/`Result`, `MetadataStore`,
//! `CommitOutcome`, `Precondition`, `WriteBatch`); the sibling traits
//! (`ChunkStore`, `Coordination`) are not needed by the gate.

use async_trait::async_trait;
use bytes::Bytes;

/// The boxed error type used across the trait surface at Milestone 0. Concrete
/// backends surface their own error detail through it; richer typed errors are
/// a later refinement once the failure modes are pinned by an implementation.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A convenience result alias for the trait surface.
pub type Result<T> = std::result::Result<T, BoxError>;

/// The authoritative metadata store: inodes, dirents, chunk maps, the
/// pending-chunk GC ledger, and version counters.
///
/// Deliberately a **narrow key/value primitive** (ADR-0008): get, prefix scan,
/// and a single atomic [`commit`](MetadataStore::commit) of a [`WriteBatch`]
/// guarded by multi-key preconditions. Filesystem semantics — inode/dirent
/// records, version compare-and-set, the pending-chunk ledger — are expressed
/// *through* this primitive by the metadata model in `core`, never baked into
/// the trait, which keeps the layer honest about the KV features it depends on
/// and makes a backend swap (redb → TiKV) a composition change (ADR-0010).
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Read the raw value stored under `key`, if any.
    async fn get(&self, key: &[u8]) -> Result<Option<Bytes>>;

    /// Return every `(key, value)` whose key begins with `prefix`, e.g. every
    /// dirent under a parent. Order is unspecified.
    async fn scan(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Bytes)>>;

    /// Apply `batch` as a single atomic mutation — the commit point. Either
    /// every precondition holds and every put/delete lands, or nothing changes.
    /// Returns [`CommitOutcome::Conflict`] (not `Err`) when a precondition fails,
    /// so a stale writer is rejected distinguishably from a backend fault.
    async fn commit(&self, batch: WriteBatch) -> Result<CommitOutcome>;
}

/// The result of a [`commit`](MetadataStore::commit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    /// All preconditions held; the batch was applied.
    Committed,
    /// A precondition did not hold; nothing was written (e.g. a stale-version
    /// writer, or a name that already exists).
    Conflict,
}

/// A precondition the store checks atomically before applying a [`WriteBatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Precondition {
    /// The key whose current value is constrained.
    pub key: Vec<u8>,
    /// The required current value: `Some(bytes)` to require an exact match,
    /// `None` to require the key be absent.
    pub expected: Option<Bytes>,
}

/// A set of preconditions plus puts and deletes, applied atomically by
/// [`commit`](MetadataStore::commit). Build it with the helpers below.
#[derive(Debug, Clone, Default)]
pub struct WriteBatch {
    /// Conditions that must all hold for the batch to apply.
    pub preconditions: Vec<Precondition>,
    /// Keys to set to the given values.
    pub puts: Vec<(Vec<u8>, Bytes)>,
    /// Keys to remove.
    pub deletes: Vec<Vec<u8>>,
}

impl WriteBatch {
    /// An empty batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Require `key` to currently equal `value`.
    pub fn require(mut self, key: impl Into<Vec<u8>>, value: impl Into<Bytes>) -> Self {
        self.preconditions.push(Precondition {
            key: key.into(),
            expected: Some(value.into()),
        });
        self
    }

    /// Require `key` to currently be absent.
    pub fn require_absent(mut self, key: impl Into<Vec<u8>>) -> Self {
        self.preconditions.push(Precondition {
            key: key.into(),
            expected: None,
        });
        self
    }

    /// Set `key` to `value`.
    pub fn put(mut self, key: impl Into<Vec<u8>>, value: impl Into<Bytes>) -> Self {
        self.puts.push((key.into(), value.into()));
        self
    }

    /// Remove `key`.
    pub fn delete(mut self, key: impl Into<Vec<u8>>) -> Self {
        self.deletes.push(key.into());
        self
    }
}
