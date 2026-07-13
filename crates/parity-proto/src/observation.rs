//! THE LAYERED OBSERVATION.
//!
//! Nothing here is normalized at capture. Canonicalization is a **projection applied
//! at compare time** (see `project.rs`). The trace on disk keeps every raw timestamp,
//! TTL, native error string and proto field; the comparator decides what to look at,
//! and a ledger claim may *widen* that view to compare a field the default hides.
//!
//! That ordering is the whole answer to "how do you canonicalize without destroying
//! the differences you are hunting?" — **you don't canonicalize the evidence, you
//! canonicalize the question.** Evidence discarded at capture is gone; a projection
//! can always be widened after the fact.
//!
//! Three layers, and each is diffed differently:
//!
//! | layer    | content                                        | diffed by default?              |
//! |----------|------------------------------------------------|---------------------------------|
//! | `class`  | closed vocabulary, from the kvproto TiKV sent  | **always** — the claim surface  |
//! | `proto`  | the `kvrpcpb` message as JSON                  | presence + type only; fields opt-in |
//! | `native` | the client's OWN taxonomy, verbatim            | **never** — but always *printed* |
//!
//! `native` is why this papers over nothing. Finding 1 IS a native-taxonomy bug:
//! `check_txn_status` matches `Error::ExtractedErrors` but the plan shape only ever
//! delivers `Error::MultipleKeyErrors`. Both are `Class::TxnNotFound` — the same
//! *fact* — so a class-only diff would show them as identical and the bug would be
//! invisible. Keeping `native.type` means a claim can opt into diffing precisely the
//! distinction that IS the defect.

use serde::Deserialize;
use serde::Serialize;

use crate::class::Class;

/// One command's result, as the driver saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// L2 — the parity claim surface.
    #[serde(flatten)]
    pub class: Class,

    /// The value a read returned. Bytes, always base64 — never utf-8-lossy.
    /// (`gate::a2` exists precisely because values must stay opaque bytes; a lossy
    /// round-trip through a String would silently pass a test whose entire point is
    /// that it must not.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Bytes>,

    /// Locks observed by `scan_locks`. THE observation for findings 1 and 2, and the
    /// one place both clients can be asked the same question about durable residue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locks: Vec<LockObs>,

    /// Transaction timestamps. Recorded, never compared (see `project.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ts: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_ts: Option<u64>,

    /// L1 — the `kvrpcpb` message, as canonical JSON, when the error carried one.
    ///
    /// Presence and message type are compared by default; field values are opt-in.
    /// That default is deliberate: it cheaply catches "Rust surfaced no proto at all,
    /// Go surfaced `AlreadyExist`" — the `insert`-on-existing-key divergence, where
    /// Rust's `DuplicateKeyInsertion` is a CLIENT-SIDE buffer check with no server
    /// round-trip and Go's `ErrKeyExist` comes back from prewrite — without drowning
    /// every diff in raw timestamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proto: Option<serde_json::Value>,

    /// L0 — the client's own taxonomy, verbatim. NEVER diffed by default; ALWAYS
    /// printed in a divergence report, so a human can overturn any mapping call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeObs>,
}

impl Observation {
    pub fn new(class: Class) -> Self {
        Self {
            class,
            value: None,
            locks: Vec::new(),
            start_ts: None,
            commit_ts: None,
            proto: None,
            native: None,
        }
    }

    pub fn ok() -> Self {
        Self::new(Class::Ok)
    }

    /// The driver could not do this at all, because the client cannot.
    pub fn unsupported(detail: impl Into<String>) -> Self {
        Self::new(Class::Unsupported {
            detail: detail.into(),
        })
    }

    /// The harness broke. Inadmissible; never a divergence.
    pub fn driver_error(detail: impl Into<String>) -> Self {
        Self::new(Class::DriverError {
            detail: detail.into(),
        })
    }

    pub fn with_value(mut self, v: Option<Vec<u8>>) -> Self {
        self.value = v.map(Bytes::from);
        self
    }

    pub fn with_native(mut self, native: NativeObs) -> Self {
        self.native = Some(native);
        self
    }

    pub fn with_proto(mut self, proto: serde_json::Value) -> Self {
        self.proto = Some(proto);
        self
    }

    pub fn with_locks(mut self, locks: Vec<LockObs>) -> Self {
        self.locks = locks;
        self
    }

    pub fn with_start_ts(mut self, ts: u64) -> Self {
        self.start_ts = Some(ts);
        self
    }

    pub fn with_commit_ts(mut self, ts: u64) -> Self {
        self.commit_ts = Some(ts);
        self
    }
}

/// Opaque bytes on the wire: base64, plus a `utf8` echo when printable.
///
/// Only `b64` is ever compared. `utf8` exists so a human reading a trace or a
/// divergence report can see `gate/d6/…/z-secondary` instead of a wall of base64 —
/// it is an affordance for the reader, never an input to the verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bytes {
    pub b64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utf8: Option<String>,
}

impl Bytes {
    pub fn as_slice(&self) -> Vec<u8> {
        b64_decode(&self.b64).unwrap_or_default()
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(v: Vec<u8>) -> Self {
        Self {
            b64: b64_encode(&v),
            utf8: String::from_utf8(v.clone())
                .ok()
                .filter(|s| s.chars().all(|c| !c.is_control())),
        }
    }
}

/// A lock, as `scan_locks` reported it.
///
/// Deliberately the `kvrpcpb::LockInfo` fields both clients expose, and no more:
/// this is the symmetric observation that makes findings 1 and 2 diffable at all
/// (Rust: `TransactionClient::scan_locks -> Vec<kvrpcpb::LockInfo>`;
/// Go: `tikv.StoreProbe.ScanLocks -> []*txnlock.Lock`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockObs {
    pub key: Bytes,
    pub primary: Bytes,
    /// The `kvrpcpb::Op` enum BY NAME (`put` / `del` / `lock` / `pessimistic_lock`).
    /// Identical numbering in both generated protos, so the name is exact — and a
    /// name survives a proto renumbering that a bare integer would not.
    pub kind: String,
    /// Recorded, never compared: Rust's default TTL and Go's size-scaled TTL
    /// (`SetLockTTLByTimeAndSize`) genuinely differ, and that is a difference we
    /// choose not to claim parity on. `project.rs` buckets it to {zero, positive}.
    pub ttl_ms: u64,
    pub txn_start_ts: u64,
}

/// The client's own error taxonomy — the layer that keeps the mapping honest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeObs {
    /// "rust" | "go"
    pub lang: String,
    /// The client's own type name: `MultipleKeyErrors`, `*tikverr.ErrWriteConflict`.
    pub r#type: String,
    /// `Debug`/`Error()` output, verbatim.
    pub display: String,
}

impl NativeObs {
    pub fn new(lang: &str, r#type: impl Into<String>, display: impl Into<String>) -> Self {
        Self {
            lang: lang.to_owned(),
            r#type: r#type.into(),
            display: display.into(),
        }
    }
}

// ── base64, hand-rolled ──────────────────────────────────────────────────────
// A dependency-free alphabet. The harness already refuses `rand` in favour of a
// 6-line xorshift (`harness::deterministic_bytes`); a base64 crate for ~30 lines of
// table lookup is the same trade, and every dependency in the runner is one more
// thing that has to be true for a verdict to mean anything.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn b64_decode(input: &str) -> Option<Vec<u8>> {
    let mut buf = 0u32;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for c in input.bytes() {
        if c == b'=' {
            break;
        }
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_arbitrary_bytes() {
        // Every length mod 3, plus the bytes that a utf-8 round-trip would destroy.
        for case in [
            vec![],
            vec![0x00],
            vec![0xff, 0xfe],
            vec![1, 2, 3],
            vec![0xde, 0xad, 0xbe, 0xef],
            (0u8..=255).collect::<Vec<_>>(),
        ] {
            let enc = b64_encode(&case);
            assert_eq!(b64_decode(&enc).unwrap(), case, "{enc}");
        }
    }

    #[test]
    fn bytes_keeps_non_utf8_values_as_bytes() {
        // CAS soundness (gate::a2) turns on values being byte-identical. A value that
        // is not valid utf-8 must survive; the `utf8` echo is only a reader affordance.
        let raw = vec![0xff, 0x00, 0xfe];
        let b = Bytes::from(raw.clone());
        assert_eq!(b.as_slice(), raw);
        assert!(b.utf8.is_none(), "invalid utf-8 must not be echoed");
    }

    #[test]
    fn bytes_echoes_printable_utf8_for_the_reader() {
        let b = Bytes::from(b"gate/d6/z-secondary".to_vec());
        assert_eq!(b.utf8.as_deref(), Some("gate/d6/z-secondary"));
        assert_eq!(b.as_slice(), b"gate/d6/z-secondary");
    }

    #[test]
    fn a_value_containing_a_newline_is_not_echoed() {
        // The trace is newline-delimited JSON at the transport layer; a control
        // character in the `utf8` echo is a footgun waiting for a careless reader.
        let b = Bytes::from(b"a\nb".to_vec());
        assert!(b.utf8.is_none());
    }
}
