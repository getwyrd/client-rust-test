//! The wyrd M4 **evaluation gate** for `tikv-client` (tikv/client-rust).
//!
//! Wyrd's Milestone 4 (proposal 0015) swaps the embedded redb metadata backend
//! for TiKV behind the unchanged [`traits::MetadataStore`] seam, resting
//! the production-durability tier on the pre-1.0 `tikv-client` crate. Before
//! that dependency is committed, the proposal requires an evaluation gate:
//!
//! - confirm the **locking-read** entry point (`get_for_update` /
//!   `lock_keys`) protects read-only precondition keys from write skew,
//! - confirm the **write-conflict** error path (`Error::KeyError` wrapping a
//!   `WriteConflict`) is distinguishable from genuine backend faults,
//! - confirm the client's futures are **`Send`** behind the object-safe trait,
//! - confirm values are stored **byte-identically** (the trait's CAS is
//!   value-equality over the whole record),
//! - confirm the multi-key atomic `commit(WriteBatch)` mapping holds under
//!   real contention (exactly-one-winner, all-or-nothing).
//!
//! This crate is that gate. The contract itself is **vendored** in
//! [`traits`] (copied from the wyrd repo, not depended on, so the harness
//! stays standalone). [`TikvMetadataStore`] prototypes the exact
//! `WriteBatch → one TiKV transaction` translation the future `metadata-tikv`
//! crate will use, and `tests/gate.rs` drives it against a real TiKV/PD
//! cluster (`make gate`).

pub mod traits;

use async_trait::async_trait;
use bytes::Bytes;
use tikv_client::BoundRange;
use tikv_client::CheckLevel;
use tikv_client::Key;
use tikv_client::Snapshot;
use tikv_client::Transaction;
use tikv_client::TransactionClient;
use tikv_client::TransactionOptions;
use tikv_client::Value;

use crate::traits::BoxError;
use crate::traits::CommitOutcome;
use crate::traits::MetadataStore;
use crate::traits::Result as WyrdResult;
use crate::traits::WriteBatch;

/// How the store protects `WriteBatch` preconditions from **write skew**.
///
/// TiKV snapshot isolation conflict-checks only keys a transaction *writes*;
/// a key it only *reads* is not checked. A precondition on a key the batch
/// does not also write (the `rename` source pattern) must therefore be locked
/// — proposal 0015's "mandatory rule".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// M4's default: `begin_pessimistic`, every precondition key read with
    /// the locking read `get_for_update`.
    Pessimistic,
    /// The measured alternative: `begin_optimistic`, every precondition key
    /// added to the prewrite lock set with `lock_keys`, so commit
    /// conflict-checks it like a written key.
    OptimisticLocked,
    /// **UNSAFE.** Plain optimistic reads for preconditions — exists only so
    /// the gate can demonstrate the write-skew anomaly the two modes above
    /// prevent (`c3` in `tests/gate.rs`). Never use this in `metadata-tikv`.
    OptimisticUnlocked,
}

/// Internal page size for [`MetadataStore::scan`]. The trait materializes the
/// whole prefix into a `Vec`, but the range read is paged so one scan is never
/// a single unbounded network request — the shape of proposal 0015's interim
/// large-directory guard.
pub const SCAN_PAGE: u32 = 256;

/// The prototype `MetadataStore` over TiKV's transactional API — the exact
/// mapping proposal 0015 specifies for `metadata-tikv`:
///
/// - `commit(WriteBatch)` = one transaction: read every precondition key under
///   [`LockMode`]'s locking discipline, byte-compare against `expected`, roll
///   back to `Ok(Conflict)` on the first mismatch, otherwise buffer every
///   put/delete and two-phase commit.
/// - A TiKV **write-write race** lost at commit folds into `Ok(Conflict)`
///   (a lost race *is* the trait's conflict); everything else stays `Err`.
/// - `get`/`scan` read from a point-in-time snapshot; `scan` is a native
///   range scan over `[prefix, prefix_upper)`, paged and materialized.
/// - The **raw API is never used**: `RawClient::compare_and_swap` is
///   single-key and cannot express the multi-key inode+dirent atomicity.
#[derive(Clone)]
pub struct TikvMetadataStore {
    client: TransactionClient,
    mode: LockMode,
}

impl TikvMetadataStore {
    /// Connect to the cluster through PD at `pd_addrs`.
    pub async fn connect(
        pd_addrs: Vec<String>,
        mode: LockMode,
    ) -> tikv_client::Result<TikvMetadataStore> {
        let client = TransactionClient::new(pd_addrs).await?;
        Ok(TikvMetadataStore { client, mode })
    }

    /// The lock mode this store runs its commits under.
    pub fn mode(&self) -> LockMode {
        self.mode
    }

    fn txn_options(&self) -> TransactionOptions {
        let options = match self.mode {
            LockMode::Pessimistic => TransactionOptions::new_pessimistic(),
            LockMode::OptimisticLocked | LockMode::OptimisticUnlocked => {
                TransactionOptions::new_optimistic()
            }
        };
        // `commit` resolves every transaction explicitly; Warn (not the
        // default Panic) so a missed path in a fault branch degrades to a
        // loud log instead of aborting the whole test binary.
        options.drop_check(CheckLevel::Warn)
    }

    /// A read snapshot at a fresh PD timestamp.
    ///
    /// A snapshot's inner transaction must be `ReadOnly` or its `Drop` would
    /// panic under the default `CheckLevel::Panic` (a snapshot is never
    /// committed or rolled back). `TransactionClient::snapshot` already
    /// applies `.read_only()` itself, so the explicit call here is redundant
    /// — kept purely as documentation of the requirement.
    async fn snapshot(&self) -> tikv_client::Result<Snapshot> {
        let ts = self.client.current_timestamp().await?;
        Ok(self
            .client
            .snapshot(ts, TransactionOptions::new_optimistic().read_only()))
    }
}

#[async_trait]
impl MetadataStore for TikvMetadataStore {
    async fn get(&self, key: &[u8]) -> WyrdResult<Option<Bytes>> {
        let mut snap = self.snapshot().await.map_err(box_err)?;
        let value = snap.get(key.to_vec()).await.map_err(box_err)?;
        Ok(value.map(Bytes::from))
    }

    async fn scan(&self, prefix: &[u8]) -> WyrdResult<Vec<(Vec<u8>, Bytes)>> {
        let mut snap = self.snapshot().await.map_err(box_err)?;
        let upper: Option<Key> = prefix_upper_bound(prefix).map(Key::from);
        let mut out: Vec<(Vec<u8>, Bytes)> = Vec::new();
        let mut cursor = Key::from(prefix.to_vec());
        loop {
            let range: BoundRange = match upper.clone() {
                Some(upper) => (cursor.clone()..upper).into(),
                // Prefix of all-0xFF bytes: no exclusive upper bound exists.
                None => (cursor.clone()..).into(),
            };
            let page: Vec<_> = snap
                .scan(range, SCAN_PAGE)
                .await
                .map_err(box_err)?
                .collect();
            let full_page = page.len() == SCAN_PAGE as usize;
            for pair in page {
                let (key, value): (Key, Value) = pair.into();
                out.push((key.into(), Bytes::from(value)));
            }
            if !full_page {
                return Ok(out);
            }
            // Resume at the smallest key strictly above the last returned one.
            let mut next = out.last().expect("page was non-empty").0.clone();
            next.push(0x00);
            cursor = Key::from(next);
        }
    }

    async fn commit(&self, batch: WriteBatch) -> WyrdResult<CommitOutcome> {
        let mut txn = self
            .client
            .begin_with_options(self.txn_options())
            .await
            .map_err(box_err)?;
        match stage(&mut txn, self.mode, &batch).await {
            // Every precondition held and every mutation is buffered: 2PC.
            Ok(true) => match txn.commit().await {
                Ok(_) => Ok(CommitOutcome::Committed),
                // Undetermined: the primary-commit RPC failed at the transport
                // layer, so the transaction MAY in fact have committed. Never
                // roll back here — doing so could TEAR a committed batch (the
                // primary stays committed while secondaries in other regions
                // get rollback records), violating all-or-nothing. Surface the
                // uncertainty as a fault; `Conflict` is wrong because it
                // promises "nothing changed", which cannot be promised.
                Err(e @ tikv_client::Error::UndeterminedError(_)) => Err(box_err(e)),
                // Roll back a failed commit — ordinary, correct use of the
                // client's API: prewrite is parallel across keys, so a commit
                // that lost a write-write race on one key may already hold
                // prewrite locks on others, and `rollback()` (legal from
                // `StartedCommit`) releases them. Any residue client-rust's
                // rollback does not itself clear — e.g. a partial *pessimistic*
                // prewrite, where `PessimisticRollback` leaves prewrite locks —
                // is left for **client-rust** to resolve on the next read
                // (lock resolution, per gate finding d6). The harness does not
                // sweep locks itself; that belongs in the client.
                Err(e) => {
                    rollback_quietly(&mut txn).await;
                    // A write-write race lost at prewrite/commit *is* the
                    // trait's Conflict — never an Err, never blindly retried.
                    if is_lost_race(&e) {
                        warn_if_masking_fault("commit", &e);
                        Ok(CommitOutcome::Conflict)
                    } else {
                        Err(box_err(e))
                    }
                }
            },
            // A precondition did not hold: nothing was written; release locks.
            Ok(false) => {
                rollback_quietly(&mut txn).await;
                Ok(CommitOutcome::Conflict)
            }
            // Staging failed. Losing a lock-acquisition race folds into
            // Conflict; anything else is a genuine backend fault.
            Err(e) => {
                rollback_quietly(&mut txn).await;
                if is_lost_race(&e) {
                    warn_if_masking_fault("stage", &e);
                    Ok(CommitOutcome::Conflict)
                } else {
                    Err(box_err(e))
                }
            }
        }
    }
}

/// Check every precondition under `mode`'s locking discipline and buffer all
/// mutations. `Ok(false)` on the first precondition that does not hold.
async fn stage(
    txn: &mut Transaction,
    mode: LockMode,
    batch: &WriteBatch,
) -> tikv_client::Result<bool> {
    for pre in &batch.preconditions {
        let actual: Option<Value> = match mode {
            // The locking read: pins the key against concurrent writers until
            // this transaction resolves, and reads the *latest committed*
            // value (not the start_ts snapshot) — proposal 0015's mandatory
            // rule for read-only precondition keys.
            LockMode::Pessimistic => txn.get_for_update(pre.key.clone()).await?,
            // Optimistic variant: the key joins the prewrite lock set, so the
            // commit conflict-checks it like a written key. The read itself
            // stays at start_ts; a concurrent writer surfaces at commit.
            LockMode::OptimisticLocked => {
                txn.lock_keys(vec![pre.key.clone()]).await?;
                txn.get(pre.key.clone()).await?
            }
            // UNSAFE demonstration mode: the read is never conflict-checked.
            LockMode::OptimisticUnlocked => txn.get(pre.key.clone()).await?,
        };
        // CAS is value-equality over the whole record: `Some` requires an
        // exact byte match, `None` requires absence.
        let holds = match (&pre.expected, &actual) {
            (Some(want), Some(got)) => want.as_ref() == got.as_slice(),
            (None, None) => true,
            _ => false,
        };
        if !holds {
            return Ok(false);
        }
    }
    for (key, value) in &batch.puts {
        txn.put(key.clone(), value.to_vec()).await?;
    }
    for key in &batch.deletes {
        txn.delete(key.clone()).await?;
    }
    Ok(true)
}

/// Best-effort rollback. Callers reach this only on a *failed or
/// preconditioned-out* commit whose outcome is determined (the
/// `UndeterminedError` path deliberately does not roll back), so the batch's
/// commit point was never reached and rolling back cannot tear a committed
/// batch. A failed rollback only leaves locks for **client-rust** to resolve
/// (lock resolution on the next read; TTL expiry), so it is logged, not
/// surfaced — the harness never sweeps locks itself.
async fn rollback_quietly(txn: &mut Transaction) {
    if let Err(e) = txn.rollback().await {
        log::warn!("rollback failed (locks left to client-rust lock resolution): {e:?}");
    }
}

fn box_err(e: tikv_client::Error) -> BoxError {
    Box::new(e)
}

/// Classify a client error as "this transaction lost a race" — the trait's
/// `Conflict` — as opposed to a genuine backend fault, which stays `Err`.
///
/// Proposal 0015 names exactly two conflict signals: an M4-detected
/// precondition mismatch (handled before commit, not here) and a TiKV
/// **write-write race** surfacing as `Error::KeyError` carrying a
/// `WriteConflict`. Everything else — network errors, region-unavailable, PD
/// timeouts, lock-resolution failures, deadlock — is deliberately *not*
/// treated as a conflict; if the gate tests surface loser paths outside this
/// classification, that is a finding to record against the proposal.
pub fn is_lost_race(err: &tikv_client::Error) -> bool {
    use tikv_client::Error;
    match err {
        Error::KeyError(key_error) => key_error.conflict.is_some(),
        // Pessimistic lock acquisition wraps the per-key error it lost on.
        Error::PessimisticLockError { inner, .. } => is_lost_race(inner),
        // A multi-key batch response: `any` is deliberate and safe — every
        // path that produces these fails *before* the commit point, so the
        // batch changed nothing regardless of the other members, and folding
        // to `Conflict` (retry-after-reread) is correct. `all` would be wrong:
        // a genuine `WriteConflict` commonly arrives beside a benign sibling
        // key error, and `all` would misreport pure contention as a fault.
        // The masking risk (a real fault hidden beside a conflict) is handled
        // observably by `warn_if_masking_fault`, not by changing this rule.
        Error::MultipleKeyErrors(errors) | Error::ExtractedErrors(errors) => {
            errors.iter().any(is_lost_race)
        }
        _ => false,
    }
}

/// Log a warning when a multi-key error folded to `Conflict` also carries a
/// member that is *not* a lost-race signal — i.e. a genuine fault masked by
/// the `any`-based classification. The fold stays safe (the batch failed
/// before its commit point), but a persistent per-key fault recurring beside
/// contention would otherwise be invisible; this surfaces it for operators.
fn warn_if_masking_fault(stage: &str, err: &tikv_client::Error) {
    use tikv_client::Error;
    let masked = match err {
        Error::MultipleKeyErrors(errors) | Error::ExtractedErrors(errors) => {
            errors.iter().any(|e| !is_lost_race(e))
        }
        _ => false,
    };
    if masked {
        log::warn!("{stage}: folding a mixed error to Conflict; a non-conflict fault sibling is masked: {err:?}");
    }
}

/// The smallest key strictly greater than every key beginning with `prefix`:
/// increment the last byte that is not `0xFF` and truncate after it. `None`
/// when no upper bound exists (empty or all-`0xFF` prefix).
pub fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper = prefix.to_vec();
    while let Some(&last) = upper.last() {
        if last == 0xFF {
            upper.pop();
            continue;
        }
        *upper.last_mut().expect("checked non-empty") = last + 1;
        return Some(upper);
    }
    None
}

// Compile-time gate obligations: the store is `Send + Sync` (shareable across
// tasks) and usable behind the object-safe trait. `#[async_trait]` boxes the
// trait futures as `Send`, so this compiling at all proves the client's
// futures are `Send` — one of proposal 0015's open-question items.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TikvMetadataStore>();
};

fn _assert_object_safe(store: &TikvMetadataStore) -> &dyn MetadataStore {
    store
}

#[cfg(test)]
mod tests {
    use super::*;
    use tikv_client::Error;
    use tikv_client::ProtoKeyError;

    #[test]
    fn prefix_upper_bound_increments_last_byte() {
        assert_eq!(prefix_upper_bound(b"abc"), Some(b"abd".to_vec()));
        assert_eq!(
            prefix_upper_bound(b"dirent:p/"),
            Some(b"dirent:p0".to_vec())
        );
    }

    #[test]
    fn prefix_upper_bound_carries_over_trailing_0xff() {
        assert_eq!(prefix_upper_bound(b"a\xff"), Some(b"b".to_vec()));
        assert_eq!(prefix_upper_bound(b"a\xff\xff"), Some(b"b".to_vec()));
    }

    #[test]
    fn prefix_upper_bound_is_unbounded_when_no_successor_exists() {
        assert_eq!(prefix_upper_bound(b""), None);
        assert_eq!(prefix_upper_bound(b"\xff\xff"), None);
    }

    fn write_conflict() -> ProtoKeyError {
        ProtoKeyError {
            conflict: Some(Default::default()),
            ..Default::default()
        }
    }

    #[test]
    fn write_conflict_key_error_is_a_lost_race() {
        assert!(is_lost_race(&Error::KeyError(Box::new(write_conflict()))));
    }

    #[test]
    fn lost_race_is_found_through_pessimistic_lock_wrapper() {
        let err = Error::PessimisticLockError {
            inner: Box::new(Error::KeyError(Box::new(write_conflict()))),
            success_keys: vec![],
        };
        assert!(is_lost_race(&err));
    }

    #[test]
    fn other_key_errors_and_faults_are_not_conflicts() {
        // A KeyError without a WriteConflict (e.g. a retryable lock message).
        let key_error = ProtoKeyError {
            retryable: "restart".to_owned(),
            ..Default::default()
        };
        assert!(!is_lost_race(&Error::KeyError(Box::new(key_error))));
        // An arbitrary fault.
        let fault = Error::StringError("region unavailable".to_owned());
        assert!(!is_lost_race(&fault));
    }

    fn txn_not_found() -> ProtoKeyError {
        ProtoKeyError {
            txn_not_found: Some(Default::default()),
            ..Default::default()
        }
    }

    #[test]
    fn a_txn_not_found_is_not_a_lost_race() {
        // An orphaned-lock `txn_not_found` is a genuine fault to surface (and
        // for client-rust to resolve), never a retryable Conflict — so it must
        // not be misclassified as a lost race.
        let poison = Error::MultipleKeyErrors(vec![Error::KeyError(Box::new(txn_not_found()))]);
        assert!(!is_lost_race(&poison));
    }
}
