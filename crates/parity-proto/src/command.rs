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
    ScanLocks {
        client: String,
        start: KeyArg,
        end: KeyArg,
        limit: u32,
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
    pub fn bytes(&self) -> Vec<u8> {
        match self {
            KeyArg::Utf8 { s } => s.as_bytes().to_vec(),
            KeyArg::B64 { b64 } => crate::observation::b64_decode(b64).unwrap_or_default(),
        }
    }

    /// Substitute the run's key prefix. Applied by the runner before dispatch.
    pub fn substitute(&self, prefix: &str) -> KeyArg {
        match self {
            KeyArg::Utf8 { s } => KeyArg::Utf8 {
                s: s.replace("{P}", prefix),
            },
            other => other.clone(),
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
    fn prefix_substitution_only_touches_utf8_args() {
        let k = KeyArg::Utf8 {
            s: "{P}z".to_owned(),
        };
        assert_eq!(k.substitute("run7/").bytes(), b"run7/z".to_vec());

        // A b64 arg is opaque bytes; substituting into it would corrupt the value.
        let raw = KeyArg::B64 {
            b64: crate::observation::b64_encode(&[0xff, 0x00]),
        };
        assert_eq!(raw.substitute("run7/").bytes(), vec![0xff, 0x00]);
    }
}
