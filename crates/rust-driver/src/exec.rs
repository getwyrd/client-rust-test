//! Command dispatch for the client-rust driver.

use std::collections::HashMap;

use parity_proto::Class;
use parity_proto::Command;
use parity_proto::Hello;
use parity_proto::LockObs;
use parity_proto::Observation;
use parity_proto::TxnMode;
use parity_proto::PROTOCOL_VERSION;
use serde::Serialize;
use tikv_client::CheckLevel;
use tikv_client::Transaction;
use tikv_client::TransactionClient;
use tikv_client::TransactionOptions;
// `Timestamp::version()` lives on this trait, not the struct.
use tikv_client::TimestampExt;

use crate::mapping::classify;

/// One reply. Exactly one field is set.
#[derive(Debug, Serialize)]
pub struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hello: Option<Hello>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
}

impl Response {
    pub fn observation(obs: Observation) -> Self {
        Self {
            hello: None,
            observation: Some(obs),
        }
    }
    fn hello(h: Hello) -> Self {
        Self {
            hello: Some(h),
            observation: None,
        }
    }
}

pub struct Driver {
    clients: HashMap<String, TransactionClient>,
    txns: HashMap<String, Transaction>,
}

impl Driver {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            txns: HashMap::new(),
        }
    }

    pub async fn execute(&mut self, cmd: Command) -> Response {
        match cmd {
            Command::Hello => Response::hello(Hello {
                driver: "rust".to_owned(),
                protocol: PROTOCOL_VERSION.to_owned(),
                client: parity_proto::command::ClientId {
                    name: "tikv-client".to_owned(),
                    // A path dependency carries no version Cargo can report, so the
                    // driver cannot witness its own revision the way the Go driver
                    // can via ReadBuildInfo. That hole is closed OUT OF BAND, by
                    // scripts/provenance.sh recording the sibling checkout's git rev
                    // and refusing to run off-pin under PARITY_STRICT.
                    version: "path:../client-rust (see results/provenance.json)".to_owned(),
                    replaced: false,
                },
                features: vec!["scan_locks".to_owned()],
                // DECLARE THE DEVIATION. Dropping an uncommitted Transaction PANICS by
                // default (CheckLevel::Panic), which would kill the driver mid-scenario
                // and make `abandon` — "the crash" — unimplementable. So every txn is
                // begun with drop_check(Warn). That is a difference from a stock client
                // and it belongs in the trace, not in a code comment nobody reads six
                // months from now.
                config: vec!["drop_check=warn (stock default is panic)".to_owned()],
            }),

            Command::OpenClient { name } => {
                match TransactionClient::new(harness::pd_addrs()).await {
                    Ok(c) => {
                        self.clients.insert(name, c);
                        Response::observation(Observation::ok())
                    }
                    Err(e) => Response::observation(Observation::driver_error(format!(
                        "open_client: {e}"
                    ))),
                }
            }

            Command::CloseClient { name } => {
                self.clients.remove(&name);
                Response::observation(Observation::ok())
            }

            Command::Begin {
                session,
                client,
                mode,
            } => {
                let Some(c) = self.clients.get(&client) else {
                    return Response::observation(Observation::driver_error(format!(
                        "begin: no such client {client}"
                    )));
                };
                // Always state the mode explicitly. Rust's TransactionOptions::default()
                // is PESSIMISTIC while Go's Begin() is OPTIMISTIC — a difference that
                // must never leak in as an unexamined default and silently make the two
                // runs incomparable.
                let opts = match mode {
                    TxnMode::Optimistic => TransactionOptions::new_optimistic(),
                    TxnMode::Pessimistic => TransactionOptions::new_pessimistic(),
                }
                .drop_check(CheckLevel::Warn);

                match c.begin_with_options(opts).await {
                    Ok(txn) => {
                        let ts = txn.start_timestamp().version();
                        self.txns.insert(session, txn);
                        Response::observation(Observation::ok().with_start_ts(ts))
                    }
                    Err(e) => Response::observation(classify(&e)),
                }
            }

            Command::Put {
                session,
                key,
                value,
            } => match self.txns.get_mut(&session) {
                Some(txn) => match txn.put(key.bytes(), value.bytes()).await {
                    Ok(()) => Response::observation(Observation::ok()),
                    Err(e) => Response::observation(classify(&e)),
                },
                None => Response::observation(no_session(&session)),
            },

            Command::Get { session, key } => match self.txns.get_mut(&session) {
                Some(txn) => match txn.get(key.bytes()).await {
                    // Rust returns Ok(None); Go returns ErrNotExist. ONE FACT, two
                    // idioms — normalize the presentation.
                    Ok(None) => Response::observation(Observation::new(Class::NotFound)),
                    Ok(Some(v)) => Response::observation(Observation::ok().with_value(Some(v))),
                    Err(e) => Response::observation(classify(&e)),
                },
                None => Response::observation(no_session(&session)),
            },

            Command::Commit { session } => match self.txns.remove(&session) {
                Some(mut txn) => match txn.commit().await {
                    Ok(Some(ts)) => {
                        Response::observation(Observation::ok().with_commit_ts(ts.version()))
                    }
                    Ok(None) => Response::observation(Observation::ok()),
                    Err(e) => Response::observation(classify(&e)),
                },
                None => Response::observation(no_session(&session)),
            },

            Command::Rollback { session } => match self.txns.remove(&session) {
                Some(mut txn) => match txn.rollback().await {
                    Ok(()) => Response::observation(Observation::ok()),
                    Err(e) => Response::observation(classify(&e)),
                },
                None => Response::observation(no_session(&session)),
            },

            // A read OUTSIDE any transaction, at a fresh timestamp: "is the key
            // readable again?" — the parity claim in the orphaned-lock scenario.
            Command::SnapshotGet { client, key } => {
                let Some(c) = self.clients.get(&client) else {
                    return Response::observation(Observation::driver_error(format!(
                        "snapshot_get: no such client {client}"
                    )));
                };
                let ts = match c.current_timestamp().await {
                    Ok(ts) => ts,
                    Err(e) => {
                        return Response::observation(Observation::driver_error(format!(
                            "snapshot_get: current_timestamp: {e}"
                        )))
                    }
                };
                // `TransactionClient::snapshot` applies .read_only() internally
                // (client.rs:230) — a claim the harness once got WRONG and retracted.
                let mut snap = c.snapshot(ts, TransactionOptions::new_optimistic());
                match snap.get(key.bytes()).await {
                    Ok(None) => Response::observation(Observation::new(Class::NotFound)),
                    Ok(Some(v)) => Response::observation(Observation::ok().with_value(Some(v))),
                    Err(e) => Response::observation(classify(&e)),
                }
            }

            // Ground truth for durable lock residue, symmetric with the Go driver's
            // StoreProbe.ScanLocks. This is what makes findings 1 and 2 DIFFABLE.
            Command::ScanLocks {
                client,
                start,
                end,
                limit,
            } => {
                let Some(c) = self.clients.get(&client) else {
                    return Response::observation(Observation::driver_error(format!(
                        "scan_locks: no such client {client}"
                    )));
                };
                let ts = match c.current_timestamp().await {
                    Ok(ts) => ts,
                    Err(e) => {
                        return Response::observation(Observation::driver_error(format!(
                            "scan_locks: current_timestamp: {e}"
                        )))
                    }
                };
                let range = start.bytes()..end.bytes();
                match c.scan_locks(&ts, range, limit).await {
                    Ok(locks) => {
                        let locks = locks
                            .into_iter()
                            .map(|l| LockObs {
                                key: l.key.into(),
                                primary: l.primary_lock.into(),
                                kind: lock_kind(l.lock_type),
                                ttl_ms: l.lock_ttl,
                                txn_start_ts: l.lock_version,
                            })
                            .collect();
                        Response::observation(Observation::ok().with_locks(locks))
                    }
                    Err(e) => Response::observation(classify(&e)),
                }
            }

            // ── THE ASYMMETRY, STATED RATHER THAN PAPERED OVER ───────────────
            // client-go drives 2PC one phase at a time with CommitterProbe, exported
            // from a NON-test file. client-rust's mocks are #[cfg(test)] and vanish
            // from the compiled crate, so there is no way to prewrite-and-stop through
            // its public API at all.
            //
            // The driver does NOT emulate this. Emulating it (say, by racing a second
            // txn to make the prewrite fail, as gate.rs::d6 must) would be exactly the
            // "workaround for a client deficiency" the repo's governing principle
            // forbids — and it would make the two runs incomparable, because the
            // SETUP would no longer be held constant.
            //
            // So it answers `unsupported`, which is a legitimate, comparable
            // observation. And the gap it names — client-rust exposes no importable
            // test probes — is itself a finding worth filing: it is *why* d6 needs a
            // region-split trick and d7 needs a compile-time failpoint to manufacture
            // states client-go's own tests simply construct.
            Command::PrewriteOnly { .. } => Response::observation(Observation::unsupported(
                "client-rust exposes no test probes (its mocks are #[cfg(test)]), so a \
                 caller cannot drive 2PC phase-by-phase. client-go exports CommitterProbe \
                 from a non-test file. The driver does NOT emulate this: the gap is the finding.",
            )),

            // Drop the txn WITHOUT commit or rollback — "the crash". Safe only because
            // Begin set drop_check(Warn); with the stock CheckLevel::Panic this would
            // abort the driver process.
            Command::Abandon { session } => {
                self.txns.remove(&session);
                Response::observation(Observation::ok())
            }
        }
    }
}

fn no_session(session: &str) -> Observation {
    Observation::driver_error(format!("no such session: {session}"))
}

/// Render `kvrpcpb::Op` BY NAME, matching the Go driver exactly. Both clients generate
/// this enum from the same proto, so the names line up; a name also survives a
/// renumbering that a bare integer would not.
fn lock_kind(op: i32) -> String {
    match op {
        0 => "put".to_owned(),
        1 => "del".to_owned(),
        2 => "lock".to_owned(),
        3 => "rollback".to_owned(),
        4 => "insert".to_owned(),
        5 => "pessimistic_lock".to_owned(),
        6 => "check_not_exists".to_owned(),
        other => format!("op_{other}"),
    }
}
