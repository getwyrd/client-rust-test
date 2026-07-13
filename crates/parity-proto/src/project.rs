//! THE PROJECTION — canonicalize the *question*, never the *evidence*.
//!
//! A trace is captured raw. This module decides what a comparison LOOKS AT. That
//! ordering is the design: a projection can always be widened after the fact, but
//! evidence discarded at capture is gone forever.
//!
//! What is compared by default:
//!   - `obs.class`         — the parity claim surface
//!   - `obs.value`         — bytes, prefix-redacted
//!   - `obs.locks[]`       — key, primary, kind (TTL bucketed)
//!   - `obs.proto`         — PRESENCE and message type only
//!
//! What is redacted (recorded in the trace, never diffed):
//!   - the run's key prefix — the two runs share a cluster and get DIFFERENT
//!     prefixes; without redaction every key diverges and the diff is worthless
//!   - `start_ts` / `commit_ts` — different every run, by construction
//!   - lock TTL — bucketed to {zero, positive}: Rust's default and Go's size-scaled
//!     TTL genuinely differ, and that is a difference we choose not to claim on
//!   - `native.display` — cross-language message text will never match; comparing it
//!     would force normalizing it into mush, which is the actual signal-destroying move
//!
//! What is OPT-IN (a ledger claim may widen the projection to include it):
//!   - `native.type` — the client's own taxonomy. Finding 1 lives HERE: Rust's
//!     `MultipleKeyErrors` vs `ExtractedErrors` is the entire bug, and both are
//!     `Class::TxnNotFound`. A class-only diff would show them identical.
//!   - `proto` field values
//!
//! ON TIMESTAMPS: they are REDACTED, not rank-symbolized. Rank-symbolizing ("the 3rd
//! distinct ts") is a trap — if Go observes 4 distinct timestamps and Rust 5, every
//! rank after the divergence shifts and the diff explodes into noise that hides the
//! one real difference. Redaction is lossy but honest; the raw values stay in the
//! trace for a human, and a claim that genuinely needs a ts relation should assert it
//! explicitly rather than infer it from position.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;

use crate::observation::Observation;
use crate::trace::Trace;

/// What a comparison looks at, beyond the defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Spec {
    /// Paths to compare IN ADDITION to the defaults, e.g. `native.type`.
    #[serde(default)]
    pub also_compare: Vec<String>,
}

/// A trace reduced to comparable facts: step id → path → value.
///
/// A `BTreeMap` so the diff is deterministic and its order is not an artifact of
/// insertion — a verdict that changes because a HashMap iterated differently is not a
/// verdict.
pub type Canonical = BTreeMap<String, BTreeMap<String, String>>;

/// One field, at one step, where two runs disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Divergence {
    pub step: String,
    pub path: String,
    pub oracle: String,
    pub subject: String,
}

const TS_REDACTED: &str = "<ts>";
const PREFIX_TOKEN: &str = "{P}";

pub fn project(trace: &Trace, spec: &Spec) -> Canonical {
    let mut out = Canonical::new();
    for step in &trace.steps {
        let mut fields = BTreeMap::new();
        project_obs(&step.observation, &trace.prefix, spec, &mut fields);
        out.insert(step.id.clone(), fields);
    }
    out
}

fn project_obs(obs: &Observation, prefix: &str, spec: &Spec, out: &mut BTreeMap<String, String>) {
    out.insert("class".to_owned(), obs.class.tag().to_owned());

    // A capability gap must be visible in the diff, not hidden behind a bare tag:
    // `unsupported` vs `ok` is the whole claim in the API-gap inventory.
    if let crate::class::Class::Unsupported { detail } = &obs.class {
        out.insert("unsupported.detail".to_owned(), detail.clone());
    }
    if let crate::class::Class::Mixed { parts } = &obs.class {
        // Sorted: a client reporting the same set in a different ORDER is not a
        // difference in fact, and the multi-key error path makes no order guarantee.
        let mut tags: Vec<_> = parts.iter().map(|p| p.tag().to_owned()).collect();
        tags.sort();
        out.insert("class.parts".to_owned(), tags.join(","));
    }

    out.insert(
        "value".to_owned(),
        match &obs.value {
            Some(v) => redact_prefix_bytes(&v.as_slice(), prefix),
            None => "<absent>".to_owned(),
        },
    );

    // Locks: sorted by redacted key so two clients returning the same residue in a
    // different order do not read as a divergence. `scan_locks` promises no order.
    let mut locks: Vec<String> = obs
        .locks
        .iter()
        .map(|l| {
            format!(
                "key={} primary={} kind={} ttl={}",
                redact_prefix_bytes(&l.key.as_slice(), prefix),
                redact_prefix_bytes(&l.primary.as_slice(), prefix),
                l.kind,
                bucket_ttl(l.ttl_ms),
            )
        })
        .collect();
    locks.sort();
    out.insert("locks.count".to_owned(), obs.locks.len().to_string());
    out.insert("locks".to_owned(), locks.join(" | "));

    // Timestamps: presence is comparable (did a commit produce one at all?), the
    // value is not.
    out.insert(
        "commit_ts".to_owned(),
        obs.commit_ts
            .map_or("<absent>".to_owned(), |_| TS_REDACTED.to_owned()),
    );

    // Proto: presence + message type by default. This is what catches "Rust surfaced
    // no proto at all, Go surfaced AlreadyExist" — i.e. a client-side buffer check vs
    // a real server round-trip — without drowning the diff in raw timestamps.
    out.insert("proto.present".to_owned(), obs.proto.is_some().to_string());
    if let Some(p) = &obs.proto {
        out.insert("proto.type".to_owned(), proto_type(p));
    }

    // ── Opt-in paths ─────────────────────────────────────────────────────────
    for path in &spec.also_compare {
        match path.as_str() {
            "native.type" => {
                out.insert(
                    "native.type".to_owned(),
                    obs.native
                        .as_ref()
                        .map_or("<absent>".to_owned(), |n| n.r#type.clone()),
                );
            }
            "proto" => {
                out.insert(
                    "proto".to_owned(),
                    obs.proto.as_ref().map_or("<absent>".to_owned(), |p| {
                        serde_json::to_string(p).unwrap_or_default()
                    }),
                );
            }
            other => {
                // An unknown opt-in path is a typo in a ledger claim, and a typo that
                // silently compares nothing would make the claim vacuously true.
                out.insert(
                    other.to_string(),
                    "<UNKNOWN PROJECTION PATH — fix the ledger claim>".to_owned(),
                );
            }
        }
    }
}

/// The run prefix is unique per run; redact it so keys compare across runs.
///
/// Applied to values too, not just keys: a scenario may store a key as a value (a
/// dirent pointing at an inode is exactly that shape), and leaving the prefix in
/// would make it diverge for a reason that has nothing to do with either client.
fn redact_prefix_bytes(raw: &[u8], prefix: &str) -> String {
    match String::from_utf8(raw.to_vec()) {
        Ok(s) => s.replace(prefix, PREFIX_TOKEN),
        // Not utf-8: compare the bytes as base64, opaque and exact.
        Err(_) => crate::observation::b64_encode(raw),
    }
}

/// Rust's default lock TTL and Go's size-scaled TTL differ by construction, so the
/// NUMBER is not a parity claim. Whether a lock has a TTL at all is.
fn bucket_ttl(ttl_ms: u64) -> &'static str {
    if ttl_ms == 0 {
        "zero"
    } else {
        "positive"
    }
}

/// The kvproto message type carried by an error, e.g. `key_error.txn_not_found`.
fn proto_type(p: &serde_json::Value) -> String {
    fn first_key(v: &serde_json::Value, path: &mut Vec<String>) {
        if let Some(obj) = v.as_object() {
            if let Some((k, inner)) = obj.iter().next() {
                path.push(k.clone());
                first_key(inner, path);
            }
        }
    }
    let mut path = Vec::new();
    first_key(p, &mut path);
    if path.is_empty() {
        "<empty>".to_owned()
    } else {
        path.join(".")
    }
}

/// Diff two projected traces. `oracle` is client-go; `subject` is client-rust.
///
/// A step present in one and absent in the other is itself a divergence — a run that
/// stopped early must never read as agreement.
pub fn diff(oracle: &Canonical, subject: &Canonical) -> Vec<Divergence> {
    let mut out = Vec::new();
    let mut steps: Vec<&String> = oracle.keys().chain(subject.keys()).collect();
    steps.sort();
    steps.dedup();

    for step in steps {
        let empty = BTreeMap::new();
        let o = oracle.get(step).unwrap_or(&empty);
        let s = subject.get(step).unwrap_or(&empty);

        let mut paths: Vec<&String> = o.keys().chain(s.keys()).collect();
        paths.sort();
        paths.dedup();

        for path in paths {
            let ov = o.get(path).map_or("<step absent>", |v| v.as_str());
            let sv = s.get(path).map_or("<step absent>", |v| v.as_str());
            if ov != sv {
                out.push(Divergence {
                    step: step.clone(),
                    path: path.clone(),
                    oracle: ov.to_owned(),
                    subject: sv.to_owned(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::Class;
    use crate::observation::LockObs;
    use crate::observation::NativeObs;
    use crate::trace::Step;
    use crate::trace::TRACE_SCHEMA;

    fn trace(prefix: &str, steps: Vec<Step>) -> Trace {
        Trace {
            schema: TRACE_SCHEMA.to_owned(),
            scenario: "orphan".to_owned(),
            run: "r".to_owned(),
            roles: vec![],
            prefix: prefix.to_owned(),
            steps,
        }
    }

    fn step(id: &str, obs: Observation) -> Step {
        Step {
            id: id.to_owned(),
            role: "reader".to_owned(),
            op: "snapshot_get".to_owned(),
            observation: obs,
        }
    }

    #[test]
    fn differing_run_prefixes_do_not_manufacture_a_divergence() {
        // THE test for redaction. The oracle and subject runs share a cluster and so
        // get different prefixes. If the prefix leaked into the projection, EVERY key
        // would differ and the diff would be pure noise.
        let o = trace(
            "parity/orphan/111/",
            vec![step(
                "read",
                Observation::ok().with_value(Some(b"parity/orphan/111/val".to_vec())),
            )],
        );
        let s = trace(
            "parity/orphan/222/",
            vec![step(
                "read",
                Observation::ok().with_value(Some(b"parity/orphan/222/val".to_vec())),
            )],
        );
        let d = diff(
            &project(&o, &Spec::default()),
            &project(&s, &Spec::default()),
        );
        assert!(d.is_empty(), "prefix leaked into the projection: {d:?}");
    }

    #[test]
    fn the_orphaned_lock_divergence_is_visible() {
        // The actual claim: the oracle resolves the orphan and reads the value; the
        // subject cannot and surfaces TxnNotFound.
        let o = trace(
            "p/",
            vec![step(
                "reader-final-read",
                Observation::ok().with_value(Some(b"orphan".to_vec())),
            )],
        );
        let s = trace(
            "q/",
            vec![step(
                "reader-final-read",
                Observation::new(Class::TxnNotFound),
            )],
        );
        let d = diff(
            &project(&o, &Spec::default()),
            &project(&s, &Spec::default()),
        );

        let classes: Vec<_> = d.iter().filter(|x| x.path == "class").collect();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].oracle, "ok");
        assert_eq!(classes[0].subject, "txn_not_found");
    }

    #[test]
    fn native_type_is_hidden_by_default_and_visible_when_a_claim_asks() {
        // Finding 1 IS a native-taxonomy bug (MultipleKeyErrors vs ExtractedErrors),
        // and BOTH map to the same Class. So the default projection must call these
        // equal — and a claim must be able to opt in and see the difference. If the
        // default diffed native.type, every cross-language run would be noise; if it
        // could not opt in, the bug would be unstatable.
        let o = trace(
            "p/",
            vec![step(
                "r",
                Observation::new(Class::TxnNotFound).with_native(NativeObs::new(
                    "go",
                    "*errors.fundamental",
                    "txn 1 not found",
                )),
            )],
        );
        let s = trace(
            "p/",
            vec![step(
                "r",
                Observation::new(Class::TxnNotFound).with_native(NativeObs::new(
                    "rust",
                    "MultipleKeyErrors",
                    "MultipleKeyErrors([...])",
                )),
            )],
        );

        let default = diff(
            &project(&o, &Spec::default()),
            &project(&s, &Spec::default()),
        );
        assert!(default.is_empty(), "native.type must be hidden by default");

        let spec = Spec {
            also_compare: vec!["native.type".to_owned()],
        };
        let opted = diff(&project(&o, &spec), &project(&s, &spec));
        assert_eq!(opted.len(), 1);
        assert_eq!(opted[0].path, "native.type");
        assert_eq!(opted[0].subject, "MultipleKeyErrors");
    }

    #[test]
    fn lock_ttl_is_bucketed_but_lock_residue_is_not() {
        // Finding 2's claim: after rollback, Go leaves 0 locks and Rust leaves 2.
        // The TTL numbers differ between clients by construction, so they must NOT
        // diverge — but the COUNT must.
        let lock = |ttl| LockObs {
            key: b"p/k".to_vec().into(),
            primary: b"p/pri".to_vec().into(),
            kind: "put".to_owned(),
            ttl_ms: ttl,
            txn_start_ts: 1,
        };
        let o = trace("p/", vec![step("locks", Observation::ok())]);
        let s = trace(
            "p/",
            vec![step(
                "locks",
                Observation::ok().with_locks(vec![lock(3000), lock(20_000)]),
            )],
        );
        let d = diff(
            &project(&o, &Spec::default()),
            &project(&s, &Spec::default()),
        );

        let count = d.iter().find(|x| x.path == "locks.count").unwrap();
        assert_eq!(count.oracle, "0");
        assert_eq!(count.subject, "2");
        // Both TTLs bucket to `positive`, so differing TTL numbers alone never diverge.
        assert!(s.steps[0].observation.locks.iter().all(|l| l.ttl_ms > 0));
    }

    #[test]
    fn a_missing_step_is_a_divergence_not_agreement() {
        // A run that died early must never read as "agreed".
        let o = trace(
            "p/",
            vec![step("a", Observation::ok()), step("b", Observation::ok())],
        );
        let s = trace("p/", vec![step("a", Observation::ok())]);
        let d = diff(
            &project(&o, &Spec::default()),
            &project(&s, &Spec::default()),
        );
        assert!(!d.is_empty());
        assert!(d.iter().all(|x| x.step == "b"));
        assert!(d.iter().any(|x| x.subject == "<step absent>"));
    }

    #[test]
    fn locks_returned_in_a_different_order_are_not_a_divergence() {
        let mk = |k: &[u8]| LockObs {
            key: k.to_vec().into(),
            primary: b"p/pri".to_vec().into(),
            kind: "put".to_owned(),
            ttl_ms: 3000,
            txn_start_ts: 1,
        };
        let o = trace(
            "p/",
            vec![step(
                "l",
                Observation::ok().with_locks(vec![mk(b"p/a"), mk(b"p/b")]),
            )],
        );
        let s = trace(
            "p/",
            vec![step(
                "l",
                Observation::ok().with_locks(vec![mk(b"p/b"), mk(b"p/a")]),
            )],
        );
        assert!(diff(
            &project(&o, &Spec::default()),
            &project(&s, &Spec::default())
        )
        .is_empty());
    }

    #[test]
    fn proto_presence_catches_a_client_side_check_vs_a_server_round_trip() {
        // `insert` on an existing key: Rust's DuplicateKeyInsertion is a client-side
        // buffer check with NO proto; Go's ErrKeyExist comes back from prewrite with
        // an AlreadyExist proto. Same Class::KeyExists — different fact, and the
        // default projection must catch it.
        let o = trace(
            "p/",
            vec![step(
                "insert",
                Observation::new(Class::KeyExists)
                    .with_proto(serde_json::json!({"key_error": {"already_exist": {}}})),
            )],
        );
        let s = trace(
            "p/",
            vec![step("insert", Observation::new(Class::KeyExists))],
        );
        let d = diff(
            &project(&o, &Spec::default()),
            &project(&s, &Spec::default()),
        );
        assert!(d
            .iter()
            .any(|x| x.path == "proto.present" && x.oracle == "true" && x.subject == "false"));
    }
}
