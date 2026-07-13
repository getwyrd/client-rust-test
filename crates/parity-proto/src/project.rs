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

/// Every path a scenario may name in `also_compare`.
///
/// Closed, and validated BEFORE a run (see `Spec::validate`). A typo here used to be
/// silently harmless in the worst possible way: an unknown path projected the same
/// sentinel string on BOTH sides, so `diff` saw them as equal, reported nothing, and the
/// comparison the claim explicitly asked for was never made. A scenario could then pass
/// its ledger check on its OTHER declared divergences while the field it named as
/// evidence went unexamined — a vacuous result wearing the costume of a verified one,
/// which is exactly the failure this harness is built to make impossible.
///
/// So an unknown path is now a hard error at load time, and adding one means adding it
/// here and teaching `project_obs` to emit it.
pub const OPT_IN_PATHS: &[&str] = &["native.type", "proto"];

/// What a comparison looks at, beyond the defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Spec {
    /// Paths to compare IN ADDITION to the defaults, e.g. `native.type`.
    #[serde(default)]
    pub also_compare: Vec<String>,
}

impl Spec {
    /// Reject an unknown opt-in path. Call this at scenario load, never at diff time —
    /// a comparison that silently does not happen is worse than one that fails.
    pub fn validate(&self) -> Result<(), String> {
        for path in &self.also_compare {
            if !OPT_IN_PATHS.contains(&path.as_str()) {
                return Err(format!(
                    "`also_compare` names `{path}`, which is not a projection path. \
                     Known paths: {}. An unknown path would compare NOTHING while looking \
                     like it compared something.",
                    OPT_IN_PATHS.join(", ")
                ));
            }
        }
        Ok(())
    }
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
                // Unreachable: Spec::validate rejects unknown paths at scenario load.
                //
                // Belt and braces, because the failure mode here is the nastiest one
                // available. Emitting a sentinel would put the SAME value on both sides,
                // so `diff` would call them equal and report nothing — the requested
                // comparison silently would not happen, and the scenario could still pass
                // its ledger check on its other divergences. A claim that examined
                // nothing would look exactly like a claim that held. Panic instead: an
                // absent comparison must never be able to masquerade as a passing one.
                panic!(
                    "unknown projection path `{other}` reached project_obs; \
                     Spec::validate should have rejected it at load time. \
                     Known paths: {}",
                    OPT_IN_PATHS.join(", ")
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
///
/// REDACTION HAPPENS ON BYTES, BEFORE ANY UTF-8 DECISION. That ordering is the whole
/// point. A binary key is namespaced by prepending the prefix BYTES (`KeyArg::as_key`),
/// and the result is generally not valid UTF-8 — so a redactor that only handled the
/// UTF-8 case would fall through to base64-encoding the key *with the run prefix still
/// in it*. Two runs get different prefixes by construction, so the oracle's and the
/// subject's otherwise-identical locks would encode differently and diverge for a reason
/// that is purely an artifact of the harness. Redact first, then choose how to render.
fn redact_prefix_bytes(raw: &[u8], prefix: &str) -> String {
    let redacted = replace_bytes(raw, prefix.as_bytes(), PREFIX_TOKEN.as_bytes());
    match String::from_utf8(redacted.clone()) {
        Ok(s) => s,
        // Not utf-8 even after redaction: compare as base64 — opaque, exact, and now
        // free of the run-specific prefix.
        Err(_) => crate::observation::b64_encode(&redacted),
    }
}

/// Byte-level find/replace of every occurrence of `needle`.
fn replace_bytes(haystack: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return haystack.to_vec();
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if i + needle.len() <= haystack.len() && &haystack[i..i + needle.len()] == needle {
            out.extend_from_slice(with);
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    out
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

/// The kvproto MESSAGE TYPE carried by an error, e.g. `key_error.txn_not_found`.
///
/// Exactly two levels — container and message — and never further. The encoding is
/// defined by the drivers themselves (both halves), and it is always
/// `{container: {message: payload}}`.
///
/// Descending into the payload would be a bug with teeth: the drivers do not carry the
/// same payload FIELDS (Rust's TxnNotFound includes `start_ts`, Go's carries an empty
/// object because client-go leaves that KeyError untyped and has no proto to lift the
/// fields from). A recursive walk would render those as
/// `key_error.txn_not_found.start_ts` vs `key_error.txn_not_found`, and two clients
/// reporting THE SAME MESSAGE would diverge purely because one attached a field. That
/// is a false divergence manufactured by the harness — the precise thing this file
/// exists to avoid.
///
/// Payload values are still comparable: a claim opts into the full `proto` path.
fn proto_type(p: &serde_json::Value) -> String {
    let Some(obj) = p.as_object() else {
        return "<malformed>".to_owned();
    };
    let Some((container, inner)) = obj.iter().next() else {
        return "<empty>".to_owned();
    };
    match inner.as_object().and_then(|m| m.keys().next()) {
        Some(message) => format!("{container}.{message}"),
        // A container whose value is a scalar (e.g. `retryable: "…"`): the container
        // IS the message.
        None => container.clone(),
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
            provenance: serde_json::Value::Null,
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
    fn a_namespaced_binary_key_is_redacted_at_the_byte_level() {
        // REGRESSION, and the closing of a loop. `KeyArg::as_key` namespaces a binary key
        // by prepending the run prefix BYTES, and the result is not valid utf-8. A
        // redactor that only handled the utf-8 case fell through to base64-encoding the
        // key WITH THE RUN PREFIX STILL IN IT — so the oracle's and the subject's
        // otherwise-identical locks encoded differently and diverged for a reason that was
        // purely an artifact of the harness.
        //
        // This is the full round trip: namespace the same binary key into two runs, and
        // the projection must call them equal.
        let raw_key = [0xffu8, 0x00];
        let key_a = crate::KeyArg::B64 {
            b64: crate::observation::b64_encode(&raw_key),
        }
        .as_key("runA/")
        .bytes();
        let key_b = crate::KeyArg::B64 {
            b64: crate::observation::b64_encode(&raw_key),
        }
        .as_key("runB/")
        .bytes();
        assert_ne!(
            key_a, key_b,
            "precondition: the runs must use different keys"
        );

        let lock = |k: Vec<u8>| LockObs {
            key: k.into(),
            primary: b"pri".to_vec().into(),
            kind: "put".to_owned(),
            ttl_ms: 3000,
            txn_start_ts: 1,
        };
        let a = trace(
            "runA/",
            vec![step("l", Observation::ok().with_locks(vec![lock(key_a)]))],
        );
        let b = trace(
            "runB/",
            vec![step("l", Observation::ok().with_locks(vec![lock(key_b)]))],
        );

        let d = diff(
            &project(&a, &Spec::default()),
            &project(&b, &Spec::default()),
        );
        assert!(
            d.is_empty(),
            "the run prefix leaked into a binary key's projection: {d:?}"
        );
    }

    #[test]
    fn proto_type_is_the_message_type_and_never_descends_into_the_payload() {
        // REGRESSION. A recursive walk rendered Rust's TxnNotFound (which carries
        // start_ts) as `key_error.txn_not_found.start_ts` and Go's (an empty object,
        // because client-go leaves that KeyError untyped) as `key_error.txn_not_found`.
        // Two clients reporting THE SAME MESSAGE would then diverge purely because one
        // attached a payload field — a false divergence manufactured by the harness.
        let rust = trace(
            "p/",
            vec![step(
                "r",
                Observation::new(Class::TxnNotFound).with_proto(
                    serde_json::json!({"key_error": {"txn_not_found": {"start_ts": 449}}}),
                ),
            )],
        );
        let go = trace(
            "p/",
            vec![step(
                "r",
                Observation::new(Class::TxnNotFound)
                    .with_proto(serde_json::json!({"key_error": {"txn_not_found": {}}})),
            )],
        );

        let d = diff(
            &project(&rust, &Spec::default()),
            &project(&go, &Spec::default()),
        );
        assert!(
            d.is_empty(),
            "same message, different payload — must NOT diverge by default: {d:?}"
        );

        let p = project(&rust, &Spec::default());
        assert_eq!(p["r"]["proto.type"], "key_error.txn_not_found");
    }

    #[test]
    fn payload_differences_are_still_visible_when_a_claim_opts_in() {
        // The other half of the above: hiding the payload from `proto.type` must not
        // make it unreachable — a claim can still compare the full proto.
        let a = trace(
            "p/",
            vec![step(
                "r",
                Observation::new(Class::TxnNotFound)
                    .with_proto(serde_json::json!({"key_error": {"txn_not_found": {}}})),
            )],
        );
        let b = trace(
            "p/",
            vec![step(
                "r",
                Observation::new(Class::TxnNotFound).with_proto(
                    serde_json::json!({"key_error": {"txn_not_found": {"start_ts": 7}}}),
                ),
            )],
        );
        let spec = Spec {
            also_compare: vec!["proto".to_owned()],
        };
        assert!(!diff(&project(&a, &spec), &project(&b, &spec)).is_empty());
    }

    #[test]
    fn an_unknown_opt_in_path_is_rejected_rather_than_silently_compared() {
        // THE VACUOUS-PASS BUG. An unknown path used to project the same sentinel on
        // BOTH sides, so diff() saw equality, reported nothing, and the comparison the
        // claim explicitly asked for never happened — while the scenario could still
        // pass its ledger check on its other divergences. A claim that examined nothing
        // looked exactly like a claim that held.
        let bad = Spec {
            also_compare: vec!["native.typo".to_owned()],
        };
        let err = bad
            .validate()
            .expect_err("an unknown path must be rejected");
        assert!(err.contains("native.typo"), "{err}");

        for good in OPT_IN_PATHS {
            Spec {
                also_compare: vec![(*good).to_owned()],
            }
            .validate()
            .unwrap_or_else(|e| panic!("`{good}` must be accepted: {e}"));
        }
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
