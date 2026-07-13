//! THE COMMAND PROTOCOL — newline-delimited JSON, request → response, one at a time.
//!
//! A driver holds named **sessions** (clients, transactions) and executes commands
//! against them. Because both drivers speak this, a scenario is written ONCE and can
//! run all-Rust, all-Go, or with roles bound to *different* clients — which is what
//! makes cross-client interop expressible at all.
//!
//! The command set is deliberately small. Every command here is one both clients can
//! *attempt*; where one cannot, it answers `Class::Unsupported` rather than the
//! driver emulating it. `prewrite_only` is the sharp case: client-go implements it
//! with `CommitterProbe`, client-rust has no probe surface at all and says so — and
//! that asymmetry is itself a finding worth filing, not a hole to paper over.

use serde::Deserialize;
use serde::Serialize;

/// A request from the runner to a driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Identify yourself. MUST be the first command. The reply is what
    /// `ledger-check` uses to refuse a run against the wrong client — including a
    /// `replace`d one.
    Hello,

    /// Open a client against `$PD_ADDRS`.
    OpenClient {
        name: String,
    },
    CloseClient {
        name: String,
    },

    /// Begin a transaction.
    ///
    /// NOTE the Rust driver MUST pass `drop_check(CheckLevel::Warn)`: dropping an
    /// uncommitted `Transaction` PANICS by default (`CheckLevel::Panic`), which would
    /// take the driver process down mid-scenario and make `abandon` unimplementable.
    /// That is a deviation from stock and the driver reports it in `hello.config`.
    Begin {
        session: String,
        client: String,
        mode: TxnMode,
    },

    Put {
        session: String,
        key: KeyArg,
        value: KeyArg,
    },
    Get {
        session: String,
        key: KeyArg,
    },
    Commit {
        session: String,
    },
    Rollback {
        session: String,
    },

    /// A read outside any transaction, at the current timestamp. This is the
    /// observation that answers "is the key readable again?" — the parity claim in
    /// the orphaned-lock scenario.
    SnapshotGet {
        client: String,
        key: KeyArg,
    },

    /// Ground truth for durable lock residue. Symmetric across both clients
    /// (Rust: `TransactionClient::scan_locks`; Go: `tikv.StoreProbe.ScanLocks`),
    /// which is what makes findings 1 and 2 diffable rather than merely assertable.
    ///
    /// CONTRACT: a driver MUST return **every lock in `[start, end)`** — never a
    /// truncated view. `batch_size` is a PAGING HINT, not a limit, and it is named that
    /// way on purpose: `TransactionClient::scan_locks` takes a per-region batch size and
    /// does NOT page within a region, while client-go's probe pages to exhaustion. A
    /// driver that passed this through as a cap would answer a different question from
    /// its counterpart and manufacture a lock-count divergence out of harness semantics
    /// rather than client behaviour. Each driver pages as its client requires.
    ScanLocks {
        client: String,
        start: KeyArg,
        end: KeyArg,
        batch_size: u32,
    },

    /// Prewrite a chosen subset of keys and STOP — no commit, no rollback.
    ///
    /// THE DETERMINISTIC STATE FACTORY. Prewriting only a secondary, with a primary
    /// that is never written, leaves exactly finding 1's orphan: a lock whose primary
    /// has no lock and no write record, so any reader must escalate to
    /// `rollback_if_not_exist` once the TTL expires.
    ///
    /// client-go implements this with `CommitterProbe` (`SetPrimaryKey`,
    /// `MutationsOfKeys`, `PrewriteMutations` — all in NON-test files, hence
    /// importable). client-rust exposes no equivalent and returns `Unsupported`.
    ///
    /// Why this matters more than it looks: it holds the SETUP CONSTANT across both
    /// runs, so any divergence is attributable to the reader alone. `gate::d6` cannot
    /// do that — it manufactures the orphan with a region split and a racing txn, so
    /// "I could not build the orphan" and "the client could not resolve it" are the
    /// same red, which is the WRONG-FAILURE hazard in person.
    PrewriteOnly {
        session: String,
        primary: KeyArg,
        keys: Vec<KeyArg>,
    },

    /// Drop the transaction WITHOUT committing or rolling back — "the crash".
    Abandon {
        session: String,
    },
}

impl Command {
    /// Every byte-string argument this command carries, in any position.
    ///
    /// Used by scenario validation to reject malformed base64 before a single command is
    /// dispatched.
    pub fn args(&self) -> Vec<&KeyArg> {
        match self {
            Command::Put { key, value, .. } => vec![key, value],
            Command::Get { key, .. } | Command::SnapshotGet { key, .. } => vec![key],
            Command::ScanLocks { start, end, .. } => vec![start, end],
            Command::PrewriteOnly { primary, keys, .. } => {
                let mut v = vec![primary];
                v.extend(keys.iter());
                v
            }
            Command::Hello
            | Command::OpenClient { .. }
            | Command::CloseClient { .. }
            | Command::Begin { .. }
            | Command::Commit { .. }
            | Command::Rollback { .. }
            | Command::Abandon { .. } => vec![],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TxnMode {
    Optimistic,
    Pessimistic,
}

/// A byte string in a scenario: `{"s": "utf8"}` or `{"b64": "…"}`.
///
/// `{P}` in an `s` is substituted with the run's unique key prefix by the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeyArg {
    Utf8 { s: String },
    B64 { b64: String },
}

impl KeyArg {
    /// Reject malformed base64 at SCENARIO LOAD, before anything runs.
    ///
    /// `bytes()` cannot fail (it is called deep inside the drivers), so a bad `b64`
    /// silently decoded to an EMPTY byte string. Both drivers would then agree on a
    /// prefix-only key or an empty value, the diff would be clean, and the ledger would
    /// certify a scenario nobody wrote — a green result for a test that never happened.
    /// A typo in a scenario must be a loud failure, not a quiet substitution.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            KeyArg::Utf8 { .. } => Ok(()),
            KeyArg::B64 { b64 } => crate::observation::b64_decode(b64).map(|_| ()).ok_or_else(|| {
                format!("`{b64}` is not valid base64. A malformed key or value would decode to EMPTY bytes and silently test something other than what was written.")
            }),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        match self {
            KeyArg::Utf8 { s } => s.as_bytes().to_vec(),
            // Safe: `validate()` runs at scenario load and rejects malformed base64.
            KeyArg::B64 { b64 } => crate::observation::b64_decode(b64).unwrap_or_default(),
        }
    }

    /// Namespace this argument as a KEY under the run's prefix.
    ///
    /// EVERY key a scenario touches must land under the run's unique prefix, and this is
    /// not a nicety — it is what keeps the two runs of a comparison from colliding. They
    /// share one cluster, and the oracle run deliberately LEAVES RESIDUE (an orphaned
    /// lock is the entire point of G-0001). An un-namespaced key would let that residue
    /// poison the subject run, and the resulting divergence — or the resulting *absence*
    /// of one — would be an artifact of the harness rather than a fact about either
    /// client.
    ///
    /// THE INVARIANT HOLDS BY CONSTRUCTION, not by inspection. It is impossible to write
    /// an un-namespaced key:
    ///
    ///   - text with `{P}`    — the token is replaced by the prefix.
    ///   - text without `{P}` — the prefix is PREPENDED.
    ///   - binary             — the prefix BYTES are prepended.
    ///
    /// `{P}` therefore says *where* the prefix goes, never *whether*. Requiring the token
    /// and trusting scenario authors to remember it would make the isolation guarantee
    /// depend on nobody ever forgetting a five-character string in a JSON file — and the
    /// failure would not look like a mistake, it would look like a divergence.
    ///
    /// Both escapes mattered and both were once open: a text key without the token, and a
    /// binary key (which has no token to substitute and used to pass through unchanged).
    /// Either sent byte-identical keys to both runs, on a SHARED cluster, where the oracle
    /// deliberately leaves residue — an orphaned lock is the whole of G-0001. Binary keys
    /// are not exotic, either: a `0xFF` boundary key cannot be written as text, and
    /// `gate::d4` already tests exactly that boundary.
    pub fn as_key(&self, prefix: &str) -> KeyArg {
        match self {
            KeyArg::Utf8 { s } => KeyArg::Utf8 {
                s: if s.contains("{P}") {
                    s.replace("{P}", prefix)
                } else {
                    format!("{prefix}{s}")
                },
            },
            KeyArg::B64 { b64 } => {
                let mut bytes = prefix.as_bytes().to_vec();
                bytes.extend(crate::observation::b64_decode(b64).unwrap_or_default());
                KeyArg::B64 {
                    b64: crate::observation::b64_encode(&bytes),
                }
            }
        }
    }

    /// Namespace this argument as a VALUE.
    ///
    /// Text values still get `{P}` substituted — a value may legitimately CONTAIN a key
    /// (a dirent pointing at an inode is exactly that shape), and it must point at the
    /// key this run actually wrote.
    ///
    /// Binary values are passed through UNTOUCHED. They are opaque bytes and must round
    /// trip byte-identically: `gate::a2` exists precisely because the store's CAS is
    /// value-equality over a whole record, and silently prepending a prefix to a value
    /// would corrupt exactly the property under test.
    pub fn as_value(&self, prefix: &str) -> KeyArg {
        match self {
            KeyArg::Utf8 { s } => KeyArg::Utf8 {
                s: s.replace("{P}", prefix),
            },
            binary => binary.clone(),
        }
    }
}

/// The driver's reply to `Hello`.
///
/// `client.version` and `replaced` are the admissibility gate. `pins.toml` says an
/// oracle you can accidentally edit is not an oracle — and until now that was a
/// *comment*. The Go driver reports its linked module version from
/// `runtime/debug.ReadBuildInfo()`, including whether it was `Replace`d, so
/// `ledger-check` can refuse a result produced against a client-go someone can edit,
/// as reported from inside the binary that actually ran. A stray `go.work` can no
/// longer produce a result at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Which driver: "rust" | "go".
    pub driver: String,
    /// Protocol version. A one-sided bump is a hard failure, not a silent mismatch.
    pub protocol: String,
    pub client: ClientId,
    /// Capabilities this driver has (e.g. "prewrite_only", "failpoints").
    #[serde(default)]
    pub features: Vec<String>,
    /// Any deviation from a stock client, so it lands in the trace and in provenance.
    /// The Rust driver declares `drop_check=warn` here — a config difference that
    /// would otherwise silently invalidate a result months from now.
    #[serde(default)]
    pub config: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientId {
    /// "tikv-client" | "github.com/tikv/client-go/v2"
    pub name: String,
    /// The version/revision ACTUALLY linked, as the binary itself reports it —
    /// never re-read from the pins file it is being checked against.
    pub version: String,
    /// Go only: was the module `replace`d with a local tree? An oracle you can edit
    /// is not an oracle.
    #[serde(default)]
    pub replaced: bool,
}

pub const PROTOCOL_VERSION: &str = "parity-cmd/v1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_as_ndjson() {
        let cases = vec![
            Command::Hello,
            Command::OpenClient {
                name: "c".to_owned(),
            },
            Command::Begin {
                session: "t".to_owned(),
                client: "c".to_owned(),
                mode: TxnMode::Optimistic,
            },
            Command::PrewriteOnly {
                session: "t".to_owned(),
                primary: KeyArg::Utf8 {
                    s: "{P}a-primary".to_owned(),
                },
                keys: vec![KeyArg::Utf8 {
                    s: "{P}z-secondary".to_owned(),
                }],
            },
            Command::Abandon {
                session: "t".to_owned(),
            },
        ];
        for c in cases {
            let line = serde_json::to_string(&c).unwrap();
            assert!(!line.contains('\n'), "a command must be ONE ndjson line");
            assert_eq!(c, serde_json::from_str::<Command>(&line).unwrap(), "{line}");
        }
    }

    #[test]
    fn a_text_key_substitutes_the_prefix_token() {
        let k = KeyArg::Utf8 {
            s: "{P}z".to_owned(),
        };
        assert_eq!(k.as_key("run7/").bytes(), b"run7/z".to_vec());
    }

    #[test]
    fn a_text_key_without_the_token_is_namespaced_anyway() {
        // REGRESSION. Requiring `{P}` made the isolation guarantee depend on a scenario
        // author remembering five characters in a JSON file — and forgetting them would
        // not look like a mistake, it would look like a DIVERGENCE: both runs writing the
        // same key on a shared cluster, the oracle's residue poisoning the subject.
        //
        // `{P}` now says WHERE the prefix goes, never WHETHER.
        let k = KeyArg::Utf8 {
            s: "z-secondary".to_owned(),
        };
        assert_eq!(k.as_key("run7/").bytes(), b"run7/z-secondary".to_vec());
        assert_ne!(k.as_key("runA/").bytes(), k.as_key("runB/").bytes());
    }

    #[test]
    fn no_key_form_can_escape_its_run_prefix() {
        // The invariant, stated over EVERY form a key can take: two runs never touch the
        // same cluster key. If any form escapes, the runs collide and the harness reports
        // an artifact of itself as a finding.
        let forms = [
            KeyArg::Utf8 {
                s: "{P}explicit".to_owned(),
            },
            KeyArg::Utf8 {
                s: "implicit".to_owned(),
            },
            KeyArg::B64 {
                b64: crate::observation::b64_encode(&[0xff, 0x00]),
            },
        ];
        for k in &forms {
            let a = k.as_key("runA/").bytes();
            let b = k.as_key("runB/").bytes();
            assert_ne!(a, b, "a key escaped its run prefix: {k:?}");
            assert!(a.starts_with(b"runA/"), "not namespaced: {k:?}");
        }
    }

    #[test]
    fn a_binary_key_is_namespaced_under_the_run_prefix() {
        // REGRESSION, and a nasty one. A b64 key used to pass through UNCHANGED, so the
        // oracle and subject runs would write the SAME key on a SHARED cluster — and the
        // oracle deliberately leaves residue (an orphaned lock is the whole of G-0001).
        // The subject would then trip over the oracle's lock and the harness would report
        // a divergence, or hide one, purely as an artifact of itself.
        //
        // Binary keys are not hypothetical: a 0xFF-boundary key cannot be written as text.
        let key = KeyArg::B64 {
            b64: crate::observation::b64_encode(&[0xff, 0x00]),
        };
        assert_eq!(key.as_key("run7/").bytes(), b"run7/\xff\x00".to_vec());

        // The isolation property itself: two runs, two prefixes, never the same key.
        assert_ne!(key.as_key("runA/").bytes(), key.as_key("runB/").bytes());
    }

    #[test]
    fn a_binary_value_is_left_byte_identical() {
        // The other half. A value is opaque bytes and must round-trip EXACTLY: the store's
        // CAS is value-equality over a whole record (gate::a2), so prefixing a value would
        // corrupt the very property under test.
        let v = KeyArg::B64 {
            b64: crate::observation::b64_encode(&[0xff, 0x00]),
        };
        assert_eq!(v.as_value("run7/").bytes(), vec![0xff, 0x00]);
    }

    #[test]
    fn a_text_value_still_substitutes_the_token() {
        // A value may legitimately CONTAIN a key — a dirent pointing at an inode is
        // exactly that shape — and must point at the key THIS run wrote.
        let v = KeyArg::Utf8 {
            s: "points-at:{P}inode".to_owned(),
        };
        assert_eq!(
            v.as_value("run7/").bytes(),
            b"points-at:run7/inode".to_vec()
        );
    }
}
