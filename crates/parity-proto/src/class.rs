//! THE CANONICAL VOCABULARY.
//!
//! The one thing that makes a cross-language diff defensible: **both clients wrap
//! the same protobuf**. `tikv-client` surfaces `kvrpcpb::KeyError` nearly raw
//! (`Error::KeyError(Box<ProtoKeyError>)`); client-go decodes the *same* proto into
//! typed errors in `ExtractKeyErr` (client-go `error/error.go:331`). So this
//! vocabulary is **not invented — it is the server's**, and a `Class` is a claim
//! about what TiKV said, not about how a client chose to spell it.
//!
//! THE RULE THIS FILE EXISTS TO ENFORCE:
//!
//! > A driver may normalize **presentation** — how the same fact is spelled in a
//! > language's idiom. It may never normalize **fact**.
//!
//! Allowed: Go's `Get` returns `ErrNotExist` where Rust returns `Ok(None)`. Same
//! fact, different idiom, both `NotFound`.
//!
//! Forbidden: client-rust cannot resolve an orphaned lock, so its read fails where
//! client-go's succeeds. That must NEVER be retried, swept into `NotFound`, or
//! otherwise smoothed over. That difference *is* the finding, and a canonicalizer
//! that erases it has destroyed the only thing this harness exists to measure.

use serde::Deserialize;
use serde::Serialize;

/// What the server said, in a closed vocabulary shared by both drivers.
///
/// Closed on purpose. Adding a variant is a deliberate, reviewed act — see
/// [`Class::Internal`], which is what happens when you skip that act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum Class {
    /// The operation succeeded.
    Ok,
    /// The key does not exist. (Rust: `Ok(None)`. Go: `ErrNotExist`.)
    NotFound,

    // ── kvrpcpb::KeyError variants ───────────────────────────────────────────
    /// `KeyError.conflict` — the lost-race signal both clients must make
    /// classifiable, and the one the whole `CommitOutcome::Conflict` contract rests on.
    WriteConflict,
    /// `KeyError.already_exist`.
    KeyExists,
    /// `KeyError.txn_not_found` — the orphaned-lock signal (finding 1).
    TxnNotFound,
    /// `KeyError.locked` — a lock the client did not resolve.
    UnresolvedLock,
    /// `KeyError.deadlock`.
    Deadlock,
    /// `KeyError.assertion_failed`.
    AssertionFailed,
    /// `KeyError.retryable`.
    Retryable,
    /// `KeyError.abort`.
    Aborted,
    /// `KeyError.commit_ts_too_large`.
    CommitTsTooLarge,
    /// The lock-wait timed out (Go's `LockNoWait` / a bounded wait elapsing).
    LockWaitTimeout,

    // ── Transport and cluster ────────────────────────────────────────────────
    /// The commit's outcome is genuinely unknown — the primary-commit RPC failed at
    /// the transport layer, so the txn MAY have committed. Never roll back on this.
    Undetermined,
    /// An `errorpb::Error`. A CLUSTER event, not client behaviour: a trace carrying
    /// one is inadmissible and the runner retries the scenario.
    RegionError,
    /// A PD-level failure.
    PdError,
    /// A transport-level failure.
    RpcError,

    /// A multi-key error whose members DISAGREE. A distinct fact, deliberately not
    /// flattened to a "primary" error: a client that reports 1-of-3 where the other
    /// reports 3-of-3 is a VISIBLE difference, and collapsing it would hide exactly
    /// the kind of divergence this harness hunts.
    Mixed { parts: Vec<Class> },

    /// **This client has no such capability.**
    ///
    /// A first-class, comparable observation — NOT an error. The driver NEVER
    /// emulates a missing feature: asked for one client-rust lacks (`Checksum`,
    /// `LockNoWait`, fair locking), it says so, and the ledger records a capability
    /// gap. Emulating it would be precisely the "workaround for a client deficiency"
    /// the repo's governing principle forbids.
    Unsupported { detail: String },

    /// The HARNESS broke — a bad command, a lost session, a protocol violation.
    /// **Inadmissible as evidence.** Never a divergence.
    DriverError { detail: String },

    /// An error with no explicit arm in this driver's mapping table.
    ///
    /// **Inadmissible as evidence, and that is the entire point.** An unmapped error
    /// must never silently become `Ok` or `WriteConflict`; it becomes a run that
    /// `ledger-check` refuses. The taxonomy can only grow by someone deciding it
    /// should — which is `gate-verdict.sh`'s WRONG-FAILURE rule, one level down:
    /// a result that *might* be a lie is worth less than no result at all.
    Internal { detail: String },
}

impl Class {
    /// Can a run containing this observation settle a ledger claim?
    ///
    /// Three ways to answer "no", and they are different failures:
    ///   - `Internal`     the mapping table has a hole; the observation may be a lie.
    ///   - `DriverError`  the harness broke; the observation is about us, not the client.
    ///   - `RegionError`  the CLUSTER moved; the observation is about the cluster.
    ///
    /// `Unsupported` is emphatically NOT here. It is a real, comparable answer.
    pub fn is_admissible(&self) -> bool {
        match self {
            Class::Internal { .. } | Class::DriverError { .. } | Class::RegionError => false,
            Class::Mixed { parts } => parts.iter().all(Class::is_admissible),
            _ => true,
        }
    }

    /// The reason this observation cannot be evidence, for the refusal message.
    pub fn inadmissible_reason(&self) -> Option<String> {
        match self {
            Class::Internal { detail } => Some(format!(
                "an error with no arm in the driver's mapping table: {detail}. \
                 The taxonomy must be EXTENDED (a reviewed act), not silently widened — \
                 an unmapped error that became `ok` would be a false green."
            )),
            Class::DriverError { detail } => Some(format!("the harness itself failed: {detail}")),
            Class::RegionError => Some(
                "a region error — a CLUSTER event, not client behaviour. \
                 The run says nothing about either client."
                    .to_owned(),
            ),
            Class::Mixed { parts } => parts.iter().find_map(Class::inadmissible_reason),
            _ => None,
        }
    }

    /// A stable, short tag for divergence reports and ledger signatures.
    pub fn tag(&self) -> &'static str {
        match self {
            Class::Ok => "ok",
            Class::NotFound => "not_found",
            Class::WriteConflict => "write_conflict",
            Class::KeyExists => "key_exists",
            Class::TxnNotFound => "txn_not_found",
            Class::UnresolvedLock => "unresolved_lock",
            Class::Deadlock => "deadlock",
            Class::AssertionFailed => "assertion_failed",
            Class::Retryable => "retryable",
            Class::Aborted => "aborted",
            Class::CommitTsTooLarge => "commit_ts_too_large",
            Class::LockWaitTimeout => "lock_wait_timeout",
            Class::Undetermined => "undetermined",
            Class::RegionError => "region_error",
            Class::PdError => "pd_error",
            Class::RpcError => "rpc_error",
            Class::Mixed { .. } => "mixed",
            Class::Unsupported { .. } => "unsupported",
            Class::DriverError { .. } => "driver_error",
            Class::Internal { .. } => "internal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_inadmissible_classes_are_inadmissible() {
        let d = "x".to_owned();
        assert!(!Class::Internal { detail: d.clone() }.is_admissible());
        assert!(!Class::DriverError { detail: d }.is_admissible());
        assert!(!Class::RegionError.is_admissible());
    }

    #[test]
    fn unsupported_is_a_real_answer_not_a_failure() {
        // The capability-gap inventory depends on this: `unsupported` must be
        // comparable against Go's `ok`, which it cannot be if it is inadmissible.
        let c = Class::Unsupported {
            detail: "client-rust has no RawKV checksum".to_owned(),
        };
        assert!(c.is_admissible());
        assert!(c.inadmissible_reason().is_none());
    }

    #[test]
    fn a_mixed_error_hiding_an_unmapped_part_is_inadmissible() {
        // The subtle one. `Mixed` must not launder an `Internal` member into an
        // admissible whole — that would reintroduce the false green through the back
        // door, in exactly the multi-key path (`MultipleKeyErrors`) where finding 1
        // lives.
        let c = Class::Mixed {
            parts: vec![
                Class::WriteConflict,
                Class::Internal {
                    detail: "???".to_owned(),
                },
            ],
        };
        assert!(!c.is_admissible());
        assert!(c.inadmissible_reason().unwrap().contains("mapping table"));
    }

    #[test]
    fn class_round_trips_through_json() {
        for c in [
            Class::Ok,
            Class::WriteConflict,
            Class::Mixed {
                parts: vec![Class::TxnNotFound, Class::WriteConflict],
            },
            Class::Unsupported {
                detail: "no checksum".to_owned(),
            },
        ] {
            let j = serde_json::to_string(&c).unwrap();
            assert_eq!(c, serde_json::from_str::<Class>(&j).unwrap(), "{j}");
        }
    }
}
