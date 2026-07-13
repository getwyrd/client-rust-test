//! THE RUST HALF OF THE MAPPING TABLE.
//!
//! Its Go twin is `go/driver/observation.go`. Between them they define what it MEANS
//! for two clients to have done the same thing, so this is the one file where a lie
//! could hide. Four defenses, in order of how much they matter:
//!
//! 1. **Nothing is normalized at capture.** `class` is a projection of what the SERVER
//!    said; the client's own error is carried verbatim in `native`, the kvproto in
//!    `proto`. A mapping call can always be overturned from the evidence.
//! 2. **`native` is ALWAYS attached.** If this table maps two different facts onto one
//!    class, the trace still contains the difference and a ledger claim can opt into
//!    diffing `native.type`. Finding 1 lives exactly there: `MultipleKeyErrors` vs
//!    `ExtractedErrors` is the whole bug, and both are `Class::TxnNotFound`.
//! 3. **The match below is EXHAUSTIVE** — no `_ =>` arm over `tikv_client::Error`. When
//!    upstream adds a variant, this file stops compiling. A mapping table that can
//!    silently ignore a new error is not a mapping table.
//! 4. **The catch-all for unrecognized *contents* is `Internal`, which is INADMISSIBLE.**
//!    An unmapped error can never become `Ok` or `WriteConflict`; it becomes a run that
//!    `ledger-check` refuses. A result that MIGHT be a lie is worth less than no result.
//!
//! THE RULE: normalize presentation, never fact.

use parity_proto::Class;
use parity_proto::NativeObs;
use parity_proto::Observation;
use tikv_client::Error;
// `tikv_client::proto` is private; `ProtoKeyError` (= kvrpcpb::KeyError) is the public
// re-export, and it is all we need — we read the proto's fields, never name its
// submessage types.
use tikv_client::ProtoKeyError;

/// Map a `tikv_client::Error` onto the canonical vocabulary, keeping the raw error.
pub fn classify(err: &Error) -> Observation {
    let native = NativeObs::new("rust", variant_name(err), format!("{err:?}"));
    let (class, proto) = classify_inner(err);

    let mut obs = Observation::new(class).with_native(native);
    if let Some(p) = proto {
        obs = obs.with_proto(p);
    }
    // CARDINALITY SURVIVES THE COLLAPSE. A homogeneous multi-key error maps to the single
    // class its members agree on — it must, or Rust's `MultipleKeyErrors([conflict])`
    // would not compare against Go's lone `ErrWriteConflict`. But collapsing alone would
    // make ONE conflict and THREE conflicts project identically, so a client silently
    // dropping or duplicating per-key errors would be invisible — flatly contradicting the
    // rule this file states two paragraphs up. The count is kept, and a claim can compare
    // it (`errors.count`).
    if let Some(n) = error_count(err) {
        obs = obs.with_error_count(n);
    }
    obs
}

/// How many per-key errors the client surfaced, if it surfaced a set of them.
fn error_count(err: &Error) -> Option<usize> {
    match err {
        Error::MultipleKeyErrors(errs) | Error::ExtractedErrors(errs) => Some(errs.len()),
        Error::PessimisticLockError { inner, .. } => error_count(inner),
        _ => None,
    }
}

/// The class, and the kvproto message it came from (when there is one).
fn classify_inner(err: &Error) -> (Class, Option<serde_json::Value>) {
    match err {
        // ── The KeyError family: the shapes TiKV itself defines ──────────────
        // This is where the canonical vocabulary is grounded. client-go decodes the
        // SAME proto in ExtractKeyErr (error/error.go:331); we read it directly.
        Error::KeyError(ke) => classify_key_error(ke),

        // A multi-key error. Deliberately NOT flattened to a "primary" error: a
        // client reporting 1-of-3 where the other reports 3-of-3 is a VISIBLE
        // difference, and collapsing it would hide exactly the divergence we hunt.
        //
        // These two variants are the crux of finding 1. `check_txn_status` matches
        // `ExtractedErrors`, but the plan shape (retry_multi_region -> merge ->
        // extract_error) can only ever deliver `MultipleKeyErrors` — so the
        // `rollback_if_not_exist` heal path is dead code. They map to the SAME class
        // (the underlying fact is the same), and `native.type` preserves the
        // distinction that IS the bug.
        Error::MultipleKeyErrors(errs) | Error::ExtractedErrors(errs) => {
            let parts: Vec<Class> = errs.iter().map(|e| classify_inner(e).0).collect();
            match parts.as_slice() {
                [] => (
                    Class::Internal {
                        detail: "an empty multi-key error carries no fact at all".to_owned(),
                    },
                    None,
                ),
                // All members agree: report the single fact, not a one-element `mixed`.
                [first, rest @ ..] if rest.iter().all(|c| c == first) => {
                    let proto = errs.first().and_then(|e| classify_inner(e).1);
                    (first.clone(), proto)
                }
                _ => (Class::Mixed { parts }, None),
            }
        }

        // The pessimistic-lock wrapper. The inner error is the fact; unwrap it, or a
        // write conflict surfaced through a lock request would be unclassifiable —
        // which is precisely what `is_lost_race` in wyrd-gate has to cope with.
        Error::PessimisticLockError { inner, .. } => classify_inner(inner),

        // ── Undetermined: the commit MAY have landed ─────────────────────────
        // Never roll back on this; doing so could TEAR a committed batch. Both
        // clients must surface it distinguishably, and this is the arm that proves
        // whether they do.
        Error::UndeterminedError(_) => (Class::Undetermined, None),

        // A typed TxnNotFound. Note client-go has NO typed equivalent — it returns a
        // bare `errors.Errorf("txn %d not found")` (error/error.go:363), so the Go
        // driver must string-match. That asymmetry is worth an upstream note: it is
        // the one KeyError Go leaves untyped, and the one finding 1 turns on.
        Error::TxnNotFound(_) => (
            Class::TxnNotFound,
            Some(serde_json::json!({"key_error": {"txn_not_found": {}}})),
        ),

        // A lock the client did not resolve. When Rust surfaces this and Go does not,
        // that IS the finding.
        Error::ResolveLockError(_) => (
            Class::UnresolvedLock,
            Some(serde_json::json!({"key_error": {"locked": {}}})),
        ),

        // Rust's `insert` guard is a CLIENT-SIDE buffer check — it never reaches the
        // server, so there is no proto. Go's ErrKeyExist comes back from a real
        // prewrite WITH a kvrpcpb.AlreadyExist. Same class; the `proto.present`
        // divergence is what reveals they are not the same event.
        Error::DuplicateKeyInsertion => (Class::KeyExists, None),

        // ── Cluster events, not client behaviour ─────────────────────────────
        // A trace carrying one of these is inadmissible: the runner retries the
        // scenario rather than reporting a divergence that is really about the cluster.
        Error::RegionError(_)
        | Error::NoCurrentRegions
        | Error::EntryNotFoundInRegionCache
        | Error::RegionForKeyNotFound { .. }
        | Error::RegionForRangeNotFound { .. }
        | Error::RegionNotFoundInResponse { .. }
        | Error::LeaderNotFound { .. } => (Class::RegionError, None),

        // ── Transport ────────────────────────────────────────────────────────
        Error::Io(_)
        | Error::Channel(_)
        | Error::Grpc(_)
        | Error::GrpcAPI(_)
        | Error::Url(_)
        | Error::Canceled(_) => (Class::RpcError, None),

        // ── Capability gaps, stated as such ──────────────────────────────────
        // NOT errors. The driver never emulates a missing feature; it says so, and
        // the ledger records a capability gap.
        Error::Unimplemented => (
            Class::Unsupported {
                detail: "client-rust returned Error::Unimplemented".to_owned(),
            },
            None,
        ),
        Error::UnsupportedMode => (
            Class::Unsupported {
                detail: "client-rust returned Error::UnsupportedMode (raw atomic-mode gating)"
                    .to_owned(),
            },
            None,
        ),

        // ── Everything else: named, but with no counterpart in the vocabulary ──
        // These are real client errors that no scenario has yet had a reason to
        // classify. They become INADMISSIBLE rather than being guessed at. When a
        // scenario needs one, someone extends the vocabulary deliberately — which is
        // the only way a taxonomy stays honest.
        Error::InvalidTransactionType
        | Error::OperationAfterCommitError
        | Error::OnePcFailure
        | Error::NoPrimaryKey
        | Error::ColumnFamilyError(_)
        | Error::JoinError(_)
        | Error::MaxScanLimitExceeded { .. }
        | Error::InvalidSemver(_)
        | Error::KvError { .. }
        | Error::InternalError { .. }
        | Error::StringError(_)
        | Error::KeyspaceNotFound(_)
        | Error::NestedRuntimeError(_) => (
            Class::Internal {
                detail: format!(
                    "client-rust {} has no arm in the parity vocabulary. \
                     Extend it deliberately (both halves of the mapping table) — \
                     do NOT let it fall through to a plausible class.",
                    variant_name(err)
                ),
            },
            None,
        ),
    }
}

/// Read the fact straight off `kvrpcpb::KeyError` — the same proto client-go decodes.
fn classify_key_error(ke: &ProtoKeyError) -> (Class, Option<serde_json::Value>) {
    if let Some(c) = &ke.conflict {
        return (
            Class::WriteConflict,
            Some(serde_json::json!({"key_error": {"conflict": {
                "start_ts": c.start_ts,
                "conflict_ts": c.conflict_ts,
                "conflict_commit_ts": c.conflict_commit_ts,
                // The REASON matters: a woken pessimistic waiter gets
                // `PessimisticRetry`, and finding 3 turned on exactly that
                // distinction (it is genuine, and client-go does the same).
                "reason": format!("{:?}", c.reason()),
            }}})),
        );
    }
    if ke.already_exist.is_some() {
        return (
            Class::KeyExists,
            Some(serde_json::json!({"key_error": {"already_exist": {}}})),
        );
    }
    if let Some(t) = &ke.txn_not_found {
        return (
            Class::TxnNotFound,
            Some(serde_json::json!({"key_error": {"txn_not_found": {
                "start_ts": t.start_ts,
            }}})),
        );
    }
    if ke.locked.is_some() {
        return (
            Class::UnresolvedLock,
            Some(serde_json::json!({"key_error": {"locked": {}}})),
        );
    }
    if ke.deadlock.is_some() {
        return (
            Class::Deadlock,
            Some(serde_json::json!({"key_error": {"deadlock": {}}})),
        );
    }
    if ke.assertion_failed.is_some() {
        return (
            Class::AssertionFailed,
            Some(serde_json::json!({"key_error": {"assertion_failed": {}}})),
        );
    }
    if ke.commit_ts_expired.is_some() || ke.commit_ts_too_large.is_some() {
        return (
            Class::CommitTsTooLarge,
            Some(serde_json::json!({"key_error": {"commit_ts_too_large": {}}})),
        );
    }
    if !ke.retryable.is_empty() {
        return (
            Class::Retryable,
            Some(serde_json::json!({"key_error": {"retryable": ke.retryable}})),
        );
    }
    if !ke.abort.is_empty() {
        return (
            Class::Aborted,
            Some(serde_json::json!({"key_error": {"abort": ke.abort}})),
        );
    }

    // A KeyError with no field we recognize. INADMISSIBLE — never `Ok`.
    (
        Class::Internal {
            detail: format!("a kvrpcpb::KeyError with no recognized field: {ke:?}"),
        },
        None,
    )
}

/// The client's own variant name, for `native.type`. This is the layer that keeps the
/// mapping honest — finding 1 is invisible without it.
fn variant_name(err: &Error) -> &'static str {
    match err {
        Error::Unimplemented => "Unimplemented",
        Error::DuplicateKeyInsertion => "DuplicateKeyInsertion",
        Error::ResolveLockError(_) => "ResolveLockError",
        Error::InvalidTransactionType => "InvalidTransactionType",
        Error::OperationAfterCommitError => "OperationAfterCommitError",
        Error::OnePcFailure => "OnePcFailure",
        Error::NoPrimaryKey => "NoPrimaryKey",
        Error::UnsupportedMode => "UnsupportedMode",
        Error::NoCurrentRegions => "NoCurrentRegions",
        Error::EntryNotFoundInRegionCache => "EntryNotFoundInRegionCache",
        Error::Io(_) => "Io",
        Error::Channel(_) => "Channel",
        Error::Grpc(_) => "Grpc",
        Error::GrpcAPI(_) => "GrpcAPI",
        Error::Url(_) => "Url",
        Error::Canceled(_) => "Canceled",
        Error::RegionError(_) => "RegionError",
        Error::UndeterminedError(_) => "UndeterminedError",
        Error::KeyError(_) => "KeyError",
        // THE two that finding 1 turns on. Same class, different name — and the name
        // is the evidence.
        Error::ExtractedErrors(_) => "ExtractedErrors",
        Error::MultipleKeyErrors(_) => "MultipleKeyErrors",
        Error::ColumnFamilyError(_) => "ColumnFamilyError",
        Error::JoinError(_) => "JoinError",
        Error::RegionForKeyNotFound { .. } => "RegionForKeyNotFound",
        Error::RegionForRangeNotFound { .. } => "RegionForRangeNotFound",
        Error::RegionNotFoundInResponse { .. } => "RegionNotFoundInResponse",
        Error::LeaderNotFound { .. } => "LeaderNotFound",
        Error::MaxScanLimitExceeded { .. } => "MaxScanLimitExceeded",
        Error::InvalidSemver(_) => "InvalidSemver",
        Error::KvError { .. } => "KvError",
        Error::InternalError { .. } => "InternalError",
        Error::StringError(_) => "StringError",
        Error::PessimisticLockError { .. } => "PessimisticLockError",
        Error::KeyspaceNotFound(_) => "KeyspaceNotFound",
        Error::TxnNotFound(_) => "TxnNotFound",
        Error::NestedRuntimeError(_) => "NestedRuntimeError",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_error(f: impl FnOnce(&mut ProtoKeyError)) -> Error {
        let mut ke = ProtoKeyError::default();
        f(&mut ke);
        Error::KeyError(Box::new(ke))
    }

    #[test]
    fn a_write_conflict_is_classified_from_the_proto() {
        let e = key_error(|ke| {
            // Built field-by-field rather than by naming kvrpcpb::WriteConflict: the
            // `proto` module is private, and the public ProtoKeyError re-export is all
            // the mapping needs.
            ke.conflict = Some(Default::default());
            if let Some(c) = ke.conflict.as_mut() {
                c.start_ts = 5;
                c.conflict_ts = 7;
            }
        });
        let obs = classify(&e);
        assert_eq!(obs.class, Class::WriteConflict);
        assert!(
            obs.proto.is_some(),
            "the kvproto must be preserved as evidence"
        );
        assert_eq!(obs.native.unwrap().r#type, "KeyError");
    }

    #[test]
    fn multiple_and_extracted_share_a_class_but_not_a_native_type() {
        // THE finding-1 invariant, and the whole reason `native` exists.
        //
        // `check_txn_status` matches ExtractedErrors; the plan shape only ever
        // delivers MultipleKeyErrors. The underlying FACT is identical (a
        // txn_not_found), so the class must be identical — otherwise the parity diff
        // would flag a difference that is not one. But the client's own type is the
        // bug, so it must survive into the trace.
        let inner = || key_error(|ke| ke.txn_not_found = Some(Default::default()));

        let multi = classify(&Error::MultipleKeyErrors(vec![inner()]));
        let extracted = classify(&Error::ExtractedErrors(vec![inner()]));

        assert_eq!(multi.class, Class::TxnNotFound);
        assert_eq!(extracted.class, Class::TxnNotFound, "same fact, same class");

        assert_eq!(multi.native.unwrap().r#type, "MultipleKeyErrors");
        assert_eq!(extracted.native.unwrap().r#type, "ExtractedErrors");
    }

    #[test]
    fn a_homogeneous_multi_key_error_keeps_its_cardinality() {
        // REGRESSION, and it contradicted this file's own stated rule. Collapsing a
        // homogeneous multi-key error to its single class is REQUIRED for cross-client
        // comparability (Rust's MultipleKeyErrors([conflict]) has to match Go's lone
        // ErrWriteConflict) — but collapsing alone made ONE conflict and THREE conflicts
        // project identically, so a client dropping or duplicating per-key errors was
        // invisible. The count survives.
        let conflict = || key_error(|ke| ke.conflict = Some(Default::default()));

        let one = classify(&Error::MultipleKeyErrors(vec![conflict()]));
        let three = classify(&Error::MultipleKeyErrors(vec![
            conflict(),
            conflict(),
            conflict(),
        ]));

        assert_eq!(one.class, Class::WriteConflict);
        assert_eq!(three.class, Class::WriteConflict, "same fact, same class");
        assert_eq!(one.error_count, Some(1));
        assert_eq!(three.error_count, Some(3), "cardinality must survive");
    }

    #[test]
    fn a_multi_key_error_whose_members_disagree_stays_mixed() {
        // Never collapse to a "primary" error: 1-of-3 vs 3-of-3 is a real difference.
        let e = Error::MultipleKeyErrors(vec![
            key_error(|ke| ke.conflict = Some(Default::default())),
            key_error(|ke| ke.txn_not_found = Some(Default::default())),
        ]);
        match classify(&e).class {
            Class::Mixed { parts } => {
                assert!(parts.contains(&Class::WriteConflict));
                assert!(parts.contains(&Class::TxnNotFound));
            }
            other => panic!("expected Mixed, got {other:?}"),
        }
    }

    #[test]
    fn a_pessimistic_lock_wrapper_is_unwrapped_to_the_inner_fact() {
        // A write conflict surfaced through a lock request must still classify as a
        // lost race, or every pessimistic retry loop above it is wrong.
        let e = Error::PessimisticLockError {
            inner: Box::new(key_error(|ke| ke.conflict = Some(Default::default()))),
            success_keys: vec![],
        };
        assert_eq!(classify(&e).class, Class::WriteConflict);
    }

    #[test]
    fn an_unrecognized_key_error_is_inadmissible_never_ok() {
        // The load-bearing defense. An empty KeyError carries no fact we know; it
        // must NOT become Ok, and it must NOT become a plausible-looking conflict.
        let obs = classify(&key_error(|_| {}));
        assert!(matches!(obs.class, Class::Internal { .. }));
        assert!(!obs.class.is_admissible());
    }

    #[test]
    fn an_unmapped_client_error_is_inadmissible() {
        let obs = classify(&Error::StringError("who knows".to_owned()));
        assert!(
            !obs.class.is_admissible(),
            "an unmapped error must never settle a claim"
        );
    }

    #[test]
    fn undetermined_is_never_confused_with_a_failure() {
        // Misclassifying this would invite a rollback that could TEAR a committed
        // batch — the single most dangerous mapping error available to us.
        let e = Error::UndeterminedError(Box::new(Error::StringError("rpc died".to_owned())));
        assert_eq!(classify(&e).class, Class::Undetermined);
    }
}
