//! PD's HTTP API as ground truth for the cluster's region layout.
//!
//! The gate's headline obligation — a multi-key `commit(WriteBatch)` is atomic —
//! is only interesting when the batch genuinely spans Raft regions. `cluster/tikv.toml`
//! sets tiny split thresholds (`region-max-keys = 10`) so that it does, but until
//! now **nothing checked that it actually happened**: the tests simply assumed it.
//! A test whose keys silently share one region still passes, and proves nothing.
//!
//! So the multi-region property becomes a *precondition*, asserted against PD,
//! rather than an assumption. This module is the ground truth for that; it is
//! test-only and the store under test never sees it.
//!
//! # Key encoding
//!
//! PD reports region bounds in TiKV's **memcomparable** encoding, not as raw keys:
//! the key is split into 8-byte groups, each group zero-padded to 8 bytes and
//! followed by a marker byte `0xFF - pad`. Verified empirically against the v8.5.5
//! cluster — a region boundary inside our own keyspace reads as
//!
//! ```text
//! "gate/d6/" FF "17838806" FF "99008050" FF "702/m-fi" FF ...
//!  ^^^^^^^^ 8   ^^^^^^^^ 8      (marker 0xFF = a full group, 0 padding)
//! ```
//!
//! and a 4-byte key `r\0\0\0` reads as `72 00 00 00 | 00 00 00 00 | FB`
//! (`0xFF - 4` padding). There is no `z` prefix and no keyspace prefix under
//! api-v1. Comparing a *raw* key against these bounds would silently give the
//! wrong region, so encode before comparing.

#![allow(dead_code)]

use std::time::Duration;
use std::time::Instant;

use serde_json::Value;

use super::pd_addrs;

/// Encode a raw key the way PD reports region bounds (memcomparable).
pub fn encode_key(key: &[u8]) -> Vec<u8> {
    const GROUP: usize = 8;
    let mut out = Vec::with_capacity(key.len() / GROUP * (GROUP + 1) + GROUP + 1);
    for chunk in key.chunks(GROUP) {
        out.extend_from_slice(chunk);
        let pad = GROUP - chunk.len();
        out.extend(std::iter::repeat_n(0u8, pad));
        out.push(0xFF - pad as u8);
    }
    // A key whose length is an exact multiple of 8 still needs a trailing
    // all-padding group, or it would sort before its own extensions.
    if key.len().is_multiple_of(GROUP) {
        out.extend_from_slice(&[0u8; GROUP]);
        out.push(0xFF - GROUP as u8); // 0xF7
    }
    out
}

#[derive(Debug, Clone)]
pub struct RegionInfo {
    pub id: u64,
    /// Memcomparable, as PD reports it. Empty = unbounded.
    pub start: Vec<u8>,
    pub end: Vec<u8>,
}

impl RegionInfo {
    /// Does this region hold `encoded` (already memcomparable)?
    fn contains(&self, encoded: &[u8]) -> bool {
        let after_start = self.start.is_empty() || encoded >= self.start.as_slice();
        let before_end = self.end.is_empty() || encoded < self.end.as_slice();
        after_start && before_end
    }
}

/// GET from PD, trying every endpoint in `$PD_ADDRS` before giving up.
///
/// `$PD_ADDRS` is comma-separated and the client under test is handed all of them,
/// so it can happily connect through the second entry while the first is down (a
/// follower restarting, say). Reading only `pd[0]` would panic the precondition
/// checks on a cluster that is, by the client's own standard, perfectly reachable.
async fn pd_get(path: &str) -> Value {
    let addrs = pd_addrs();
    let mut errors = Vec::new();
    for addr in &addrs {
        let url = format!("http://{addr}{path}");
        match reqwest::get(&url).await {
            Ok(resp) => {
                let body = resp.text().await.expect("PD response body");
                return serde_json::from_str(&body)
                    .unwrap_or_else(|e| panic!("PD {url} returned non-JSON: {e} / {body}"));
            }
            Err(e) => errors.push(format!("  {url}: {e}")),
        }
    }
    panic!(
        "no PD endpoint answered {path} — is the cluster up? (`make cluster-up`)\n{}",
        errors.join("\n")
    );
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("PD key hex"))
        .collect()
}

/// How many Raft regions the cluster currently has.
pub async fn region_count() -> u64 {
    pd_get("/pd/api/v1/regions").await["count"]
        .as_u64()
        .expect("PD /regions has a count")
}

pub async fn regions() -> Vec<RegionInfo> {
    let v = pd_get("/pd/api/v1/regions").await;
    v["regions"]
        .as_array()
        .expect("PD /regions has a regions array")
        .iter()
        .map(|r| RegionInfo {
            id: r["id"].as_u64().unwrap_or_default(),
            start: hex_to_bytes(r["start_key"].as_str().unwrap_or("")),
            end: hex_to_bytes(r["end_key"].as_str().unwrap_or("")),
        })
        .collect()
}

/// Locate `key` within an already-fetched region snapshot.
fn locate<'a>(snapshot: &'a [RegionInfo], key: &[u8]) -> Option<&'a RegionInfo> {
    let encoded = encode_key(key);
    snapshot.iter().find(|r| r.contains(&encoded))
}

/// The region currently holding `key` (raw; encoded here before comparing).
///
/// For *two* keys use [`region_pair`] — never call this twice. See the note there.
pub async fn region_of(key: &[u8]) -> Option<RegionInfo> {
    locate(&regions().await, key).cloned()
}

/// Which regions hold `a` and `b`, **as of one PD snapshot**.
///
/// This must be a single fetch. Calling `region_of(a)` then `region_of(b)` issues
/// two independent `/regions` reads, and the layout can change between them — the
/// cluster is actively splitting, and `pd.toml`'s merge scheduler is actively
/// undoing splits. Two ids drawn from different snapshots can differ without the
/// keys ever having been in different regions *at the same moment*, which would
/// report the cross-region precondition as met when it never held. That failure
/// mode is precisely the merge race this module exists to defend against, so the
/// comparison has to come from one consistent view.
pub async fn region_pair(a: &[u8], b: &[u8]) -> (Option<u64>, Option<u64>) {
    let snapshot = regions().await;
    (
        locate(&snapshot, a).map(|r| r.id),
        locate(&snapshot, b).map(|r| r.id),
    )
}

/// Are these two keys in different regions, in a single PD view?
pub async fn are_cross_region(a: &[u8], b: &[u8]) -> bool {
    matches!(region_pair(a, b).await, (Some(x), Some(y)) if x != y)
}

/// TiKV store ids PD reports as `Up`.
pub async fn stores_up() -> Vec<u64> {
    let v = pd_get("/pd/api/v1/stores").await;
    v["stores"]
        .as_array()
        .map(|stores| {
            stores
                .iter()
                .filter(|s| s["store"]["state_name"].as_str() == Some("Up"))
                .filter_map(|s| s["store"]["id"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until a region boundary separates `lo` from `hi`, writing filler keys
/// between them to provoke the split.
///
/// Splits only happen where data is, so a boundary has to be *manufactured*
/// between the two specific keys a test cares about — a globally well-split
/// cluster says nothing about whether `lo` and `hi` are separated.
///
/// It has to be a poll, not a one-shot: `cluster/pd.toml` runs an aggressive
/// merge scheduler (`max-merge-region-size = 1`), so a freshly split pair of tiny
/// regions can be merged straight back together. On a cluster with many small
/// regions the merge can outrun the split, which is exactly how d6 used to fail
/// *before reaching its assertion* and get miscounted as proof of the bug.
///
/// Panics on timeout, naming the precondition — a test needing cross-region keys
/// must never silently degrade into a same-region test that proves nothing.
pub async fn ensure_cross_region<F, Fut>(prefix: &[u8], lo: &[u8], hi: &[u8], mut write_filler: F)
where
    F: FnMut(Vec<Vec<u8>>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    const TIMEOUT: Duration = Duration::from_secs(45);
    let deadline = Instant::now() + TIMEOUT;
    let mut round = 0u32;

    loop {
        // One snapshot decides it, and the same snapshot is what gets reported —
        // re-reading PD for the log line would print ids that never coexisted.
        if let (Some(a), Some(b)) = region_pair(lo, hi).await {
            if a != b {
                println!("cross-region precondition met after {round} round(s): {a} != {b}");
                return;
            }
        }

        assert!(
            Instant::now() < deadline,
            "PRECONDITION FAILED: no region boundary separates {lo:?} from {hi:?} after {TIMEOUT:?} \
             ({} rounds of filler, cluster has {} regions).\n\
             The test needs these keys in DIFFERENT Raft regions; without that it would pass \
             vacuously and prove nothing. Check that cluster/tikv.toml is mounted \
             (region-max-keys = 10) and that pd.toml's merge scheduler is not undoing the split.",
            round,
            region_count().await,
        );

        // Filler strictly between lo and hi, so the split checker has data to cut.
        let batch: Vec<Vec<u8>> = (0..40)
            .map(|i| {
                let mut k = prefix.to_vec();
                k.extend_from_slice(format!("m-fill/{round:03}{i:03}").as_bytes());
                k
            })
            .collect();
        write_filler(batch).await;

        round += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::encode_key;
    use super::locate;
    use super::RegionInfo;

    #[test]
    fn encodes_a_full_group_with_a_trailing_pad_group() {
        // 8 bytes exactly: one full group (marker 0xFF), then an all-pad group.
        assert_eq!(
            encode_key(b"gate/d6/"),
            [b"gate/d6/".as_slice(), &[0xFF], &[0u8; 8], &[0xF7]].concat()
        );
    }

    #[test]
    fn encodes_a_short_key_with_the_pad_marker() {
        // 4 bytes + 4 padding -> marker 0xFF - 4 = 0xFB. This is the shape PD
        // reports for its own `r\0\0\0` boundary, which is how the rule was
        // confirmed against the live cluster.
        assert_eq!(
            encode_key(b"r\0\0\0"),
            vec![b'r', 0, 0, 0, 0, 0, 0, 0, 0xFB]
        );
    }

    #[test]
    fn encoding_preserves_order() {
        // The whole point of memcomparable: byte order of the encoding must match
        // byte order of the raw keys, or region lookups land in the wrong region.
        let mut raw: Vec<&[u8]> = vec![b"a", b"ab", b"b", b"gate/d6/", b"gate/d6/z", b""];
        raw.sort();
        let mut encoded: Vec<Vec<u8>> = raw.iter().map(|k| encode_key(k)).collect();
        let expected = encoded.clone();
        encoded.sort();
        assert_eq!(
            encoded, expected,
            "memcomparable encoding must be order-preserving"
        );
    }

    /// A snapshot split at `mid`: [.., mid) and [mid, ..).
    fn snapshot(mid: &[u8]) -> Vec<RegionInfo> {
        let bound = encode_key(mid);
        vec![
            RegionInfo {
                id: 1,
                start: Vec::new(), // unbounded left
                end: bound.clone(),
            },
            RegionInfo {
                id: 2,
                start: bound,
                end: Vec::new(), // unbounded right
            },
        ]
    }

    #[test]
    fn locate_respects_region_bounds() {
        let snap = snapshot(b"m");
        // start is INCLUSIVE, end is EXCLUSIVE — the boundary key itself belongs
        // to the region that starts there, not the one that ends there.
        assert_eq!(locate(&snap, b"a").map(|r| r.id), Some(1));
        assert_eq!(locate(&snap, b"m").map(|r| r.id), Some(2));
        assert_eq!(locate(&snap, b"z").map(|r| r.id), Some(2));
    }

    #[test]
    fn locate_handles_unbounded_ends() {
        // Empty start/end mean "unbounded", NOT "the empty key" — treating them as
        // a literal bound would put every key in region 1 and quietly report every
        // pair as same-region, defeating the precondition.
        let snap = snapshot(b"m");
        assert_eq!(locate(&snap, b"").map(|r| r.id), Some(1));
        assert_eq!(locate(&snap, &[0xFF; 64]).map(|r| r.id), Some(2));
    }

    #[test]
    fn locate_separates_keys_that_straddle_a_boundary() {
        // The property d6 depends on.
        let snap = snapshot(b"m");
        let lo = locate(&snap, b"a-primary").map(|r| r.id);
        let hi = locate(&snap, b"z-secondary").map(|r| r.id);
        assert_ne!(lo, hi, "keys either side of the split must be cross-region");
    }
}
