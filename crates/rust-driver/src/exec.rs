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

            // Ground truth for durable lock residue, and it must answer the SAME QUESTION
            // as the Go driver's StoreProbe.ScanLocks: "every lock in [start, end)".
            //
            // The two clients do not offer that question at the same altitude, and the
            // difference is a trap. `TransactionClient::scan_locks`'s third argument is a
            // BATCH SIZE, not a limit: the plan (retry_multi_region -> Collect) returns up
            // to that many locks PER REGION and does not page within one. client-go's
            // probe pages to exhaustion (an internal loop at 1024/iteration). Pass a batch
            // size through naively and the two drivers silently answer different
            // questions — Rust truncating where Go does not — which would manufacture a
            // lock-count divergence out of harness semantics rather than client behaviour.
            //
            // So the Rust driver PAGES, advancing past the last key it saw, exactly as
            // client-go's own probe does. That is not papering over a client deficiency:
            // the batch API is doing precisely what it says, and both clients' test
            // helpers have to loop over it. What would be dishonest is truncating Go's
            // result to match Rust's cap — that would HIDE locks, and the whole point of
            // this observation is to see what residue is left behind.
            Command::ScanLocks {
                client,
                start,
                end,
                batch_size,
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

                if batch_size == 0 {
                    return Response::observation(Observation::driver_error(
                        "scan_locks: batch_size must be > 0 (it is a paging hint; 0 can make no progress)",
                    ));
                }

                let end_key = end.bytes();
                let mut cursor = start.bytes();
                let mut locks: Vec<LockObs> = Vec::new();

                loop {
                    let mut batch = match c
                        .scan_locks(&ts, cursor.clone()..end_key.clone(), batch_size)
                        .await
                    {
                        Ok(b) => b,
                        Err(e) => return Response::observation(classify(&e)),
                    };
                    if batch.is_empty() {
                        break;
                    }
                    batch.sort_by(|a, b| a.key.cmp(&b.key));

                    let keys: Vec<Vec<u8>> = batch.iter().map(|l| l.key.clone()).collect();
                    let Some(cut) = page_cut(&keys, batch_size as usize) else {
                        // Exhausted: no region can have been truncated.
                        locks.extend(batch.into_iter().map(to_lock_obs));
                        break;
                    };

                    locks.extend(batch.into_iter().filter(|l| l.key <= cut).map(to_lock_obs));

                    cursor = next_cursor(cut);
                    if cursor >= end_key {
                        break;
                    }
                }

                Response::observation(Observation::ok().with_locks(locks))
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

fn to_lock_obs(l: tikv_client::ProtoLockInfo) -> LockObs {
    LockObs {
        key: l.key.into(),
        primary: l.primary_lock.into(),
        kind: lock_kind(l.lock_type),
        ttl_ms: l.lock_ttl,
        txn_start_ts: l.lock_version,
    }
}

/// How far into a page of `scan_locks` results it is SAFE to commit, given that the
/// response merges per-region results each capped at `batch_size`.
///
/// Returns the key at or below which the page is provably complete, or `None` when the
/// range is exhausted and the whole page can be taken.
///
/// THE HAZARD. `TransactionClient::scan_locks` runs `retry_multi_region -> Collect`: each
/// region contributes at most `batch_size` locks and the results are merged. So a LOW
/// region can be truncated while a HIGH one is complete, which means the greatest key in
/// the merged page may belong to the high region. Advancing the cursor past that key
/// would step over the low region's unreturned tail — permanently, silently. For an
/// observation whose entire job is "what lock residue was left behind", losing locks is
/// the worst failure available: it would turn client-rust's un-resolved orphan into an
/// apparent 0-lock success and quietly close a real gap.
///
/// THE RULE, and why it is complete. If fewer than `batch_size` locks came back, no region
/// can have been truncated (a truncated region contributes exactly `batch_size`), so we
/// are done. Otherwise the safe cut is the `batch_size`-th SMALLEST key:
///
///   Let `u` be any lock in the range that was NOT returned. Then `u`'s region was
///   truncated, so that region returned exactly `batch_size` locks, all with keys below
///   `u` (a region yields its lowest keys first). All of those are in this page. Hence at
///   least `batch_size` returned keys sort below `u`, so `u > page[batch_size - 1]`.
///
/// Everything at or below that key is therefore complete. Everything above it might not
/// be, so it is dropped and re-read on the next pass.
fn page_cut(sorted_keys: &[Vec<u8>], batch_size: usize) -> Option<Vec<u8>> {
    if batch_size == 0 || sorted_keys.len() < batch_size {
        return None;
    }
    Some(sorted_keys[batch_size - 1].clone())
}

/// The next key strictly after `cut`. Appending a 0 byte works because nothing sorts
/// between `k` and `k\0`, and unlike incrementing the last byte it cannot overflow.
fn next_cursor(cut: Vec<u8>) -> Vec<u8> {
    let mut c = cut;
    c.push(0);
    c
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A cluster of regions, each holding sorted keys. Mirrors what
    /// `scan_locks(cursor..end, batch)` actually does: every region returns its LOWEST
    /// `batch` keys at or above the cursor, and the results are merged.
    fn scan(regions: &[Vec<&[u8]>], cursor: &[u8], end: &[u8], batch: usize) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = regions
            .iter()
            .flat_map(|r| {
                r.iter()
                    .filter(|k| ***k >= *cursor && ***k < *end)
                    .take(batch)
                    .map(|k| k.to_vec())
            })
            .collect();
        out.sort();
        out
    }

    /// Drive the same paging loop the driver uses, against a simulated cluster.
    fn paginate(regions: &[Vec<&[u8]>], end: &[u8], batch: usize) -> Vec<Vec<u8>> {
        let mut cursor: Vec<u8> = vec![];
        let mut got: Vec<Vec<u8>> = vec![];
        for _ in 0..100 {
            let page = scan(regions, &cursor, end, batch);
            if page.is_empty() {
                break;
            }
            match page_cut(&page, batch) {
                None => {
                    got.extend(page);
                    break;
                }
                Some(cut) => {
                    got.extend(page.into_iter().filter(|k| *k <= cut));
                    cursor = next_cursor(cut);
                    if cursor.as_slice() >= end {
                        break;
                    }
                }
            }
        }
        got.sort();
        got.dedup();
        got
    }

    #[test]
    fn a_truncated_low_region_does_not_lose_its_tail() {
        // THE BUG THIS GUARDS. Region A (low) holds 5 locks; region B (high) holds 2.
        // With batch=3, A is truncated to a1..a3 while B returns both of its keys. The
        // greatest key in the merged page is `b2` — in the HIGH region. Advancing past it
        // (the obvious "take the max" move, and what this code did at first) would skip
        // a4 and a5 forever, silently under-reporting the residue.
        let regions = vec![
            vec![
                b"a1".as_ref(),
                b"a2".as_ref(),
                b"a3".as_ref(),
                b"a4".as_ref(),
                b"a5".as_ref(),
            ],
            vec![b"b1".as_ref(), b"b2".as_ref()],
        ];
        let got = paginate(&regions, b"z", 3);
        let want: Vec<Vec<u8>> = ["a1", "a2", "a3", "a4", "a5", "b1", "b2"]
            .iter()
            .map(|k| k.as_bytes().to_vec())
            .collect();
        assert_eq!(got, want, "the low region's tail must not be skipped");
    }

    #[test]
    fn every_lock_is_returned_across_many_shapes() {
        // Exhaustive-ish: region layouts x batch sizes. Any lost key is a false "0 locks
        // remaining", which would silently close a real gap.
        let shapes: Vec<Vec<Vec<&[u8]>>> = vec![
            vec![vec![b"a1".as_ref(), b"a2".as_ref(), b"a3".as_ref()]],
            vec![vec![b"a1".as_ref()], vec![b"b1".as_ref(), b"b2".as_ref()]],
            vec![
                vec![
                    b"a1".as_ref(),
                    b"a2".as_ref(),
                    b"a3".as_ref(),
                    b"a4".as_ref(),
                ],
                vec![b"b1".as_ref()],
                vec![b"c1".as_ref(), b"c2".as_ref(), b"c3".as_ref()],
            ],
        ];
        for regions in &shapes {
            let mut want: Vec<Vec<u8>> = regions.iter().flatten().map(|k| k.to_vec()).collect();
            want.sort();
            for batch in 1..=6 {
                let got = paginate(regions, b"z", batch);
                assert_eq!(got, want, "lost locks with batch_size={batch}");
            }
        }
    }

    #[test]
    fn an_exhausted_page_needs_no_cut() {
        let keys: Vec<Vec<u8>> = vec![b"a".to_vec(), b"b".to_vec()];
        assert_eq!(
            page_cut(&keys, 5),
            None,
            "fewer than batch_size => complete"
        );
        assert_eq!(page_cut(&keys, 2), Some(b"b".to_vec()));
    }

    #[test]
    fn the_cursor_always_advances() {
        // A cursor that failed to advance would spin forever.
        let cut = b"k".to_vec();
        assert!(next_cursor(cut.clone()) > cut);
        // And it cannot overflow the way incrementing the last byte could.
        let maxed = vec![0xff, 0xff];
        assert!(next_cursor(maxed.clone()) > maxed);
    }
}
