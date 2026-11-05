//! The wait for TiKV to split a freshly written range, without any client in it.
//!
//! `p0_cluster_can_split_regions` writes a range and waits for TiKV's own split checker
//! to divide it. The control flow lives here so that it can be driven without a cluster
//! by a commit that never returns; the commits are passed in as futures, so this module
//! knows nothing of a store or a batch.
//!
//! Every await in the wait has a time limit of its own, because a deadline checked
//! between awaits is not running while one of them is pending (see `PD_TIMEOUT` in
//! `cluster.rs`). A commit's limit is what remains of the deadline, so a commit cannot
//! outlive it. That bounds the overrun; it is not a hard limit. The sleep and the PD calls
//! keep their own limits, none tied to the deadline, so a split can still be accepted one
//! sleep and one probe past the limit, and a no-split failure can arrive one region count
//! after that.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

/// What is being waited for, and how long.
#[derive(Clone, Copy, Debug)]
pub struct SplitWait {
    /// How many keys the seed commit writes; only reported.
    pub keys: usize,
    /// The deadline, counted from before the seed commit.
    pub limit: Duration,
    /// How long after a commit returns the keys are written again.
    pub rewrite_every: Duration,
    /// The sleep between split probes.
    pub poll: Duration,
}

/// Runs `seed`, then probes for a split until `wait.limit` has passed, calling `rewrite`
/// every `wait.rewrite_every`.
///
/// Returns the number of completed re-writes once `probe` reports a split, or the failure
/// message for the caller to raise. A commit that reaches the limit ends the wait at once:
/// another probe could pass past the limit, and any other PD call could delay or replace
/// the message. `region_count` is therefore called only for a no-split failure.
/// `regions_before` is the region count from before the seed commit.
pub async fn wait_for_split<S, R, RF, P, PF, C, CF>(
    wait: &SplitWait,
    regions_before: u64,
    seed: S,
    mut rewrite: R,
    mut probe: P,
    region_count: C,
) -> Result<u32, String>
where
    S: Future<Output = ()>,
    R: FnMut() -> RF,
    RF: Future<Output = ()>,
    P: FnMut() -> PF,
    PF: Future<Output = bool>,
    C: FnOnce() -> CF,
    CF: Future<Output = u64>,
{
    let deadline = Instant::now() + wait.limit;
    let mut rewrites = 0;
    limited_commit(wait, deadline, "seed", rewrites, seed).await?;
    let mut next_rewrite = Instant::now() + wait.rewrite_every;
    loop {
        if probe().await {
            return Ok(rewrites);
        }
        if Instant::now() >= deadline {
            let regions_now = region_count().await;
            return Err(no_split_message(
                wait,
                rewrites,
                regions_before,
                regions_now,
            ));
        }
        if Instant::now() >= next_rewrite {
            limited_commit(wait, deadline, "re-write", rewrites, rewrite()).await?;
            rewrites += 1;
            next_rewrite = Instant::now() + wait.rewrite_every;
        }
        tokio::time::sleep(wait.poll).await;
    }
}

/// Polls `run` until `deadline`, cancelling it there, and fails if it had not returned.
///
/// `timeout_at` polls the commit before it looks at the clock, so a commit that was ready
/// only after the deadline still comes back as `Ok`. The clock is read again for that case.
async fn limited_commit(
    wait: &SplitWait,
    deadline: Instant,
    commit: &str,
    rewrites: u32,
    run: impl Future<Output = ()>,
) -> Result<(), String> {
    let started = Instant::now();
    if tokio::time::timeout_at(deadline, run).await.is_ok() && Instant::now() < deadline {
        return Ok(());
    }
    Err(commit_limit_message(
        wait,
        commit,
        rewrites,
        started.elapsed(),
    ))
}

/// Reaching the limit does not show that a commit stalled: a healthy re-write can start just
/// before the limit. So the message says how long this one had run, and not that it stalled.
fn commit_limit_message(
    wait: &SplitWait,
    commit: &str,
    rewrites: u32,
    running: Duration,
) -> String {
    format!(
        "PRECONDITION FAILED: the {commit} commit had been running for {:.3}s and had not \
         returned when the {:?} limit for {} keys to split across regions ran out ({rewrites} \
         re-writes). That leaves open whether this range can split: a commit that ran for a \
         long time points at TiKV or PD not answering it, while one cut off soon after it \
         started was only in flight. The gate's multi-region obligations are VOID without \
         splits.",
        running.as_secs_f64(),
        wait.limit,
        wait.keys
    )
}

fn no_split_message(wait: &SplitWait, rewrites: u32, before: u64, now: u64) -> String {
    // A growing count does not prove cluster/tikv.toml is in force: other tests
    // ask PD for explicit splits. It only tells the two likely causes apart.
    let cause = if now > before {
        format!(
            "the cluster has {now} regions (was {before}), so regions are being \
             created, but this range has not split yet. Most likely the split checker \
             has not checked it even after the re-writes; if this repeats, check that \
             cluster/tikv.toml is mounted (region-max-keys = 10)"
        )
    } else {
        format!(
            "no region has split at all ({now} regions, was {before}): check that \
             cluster/tikv.toml is mounted (region-max-keys = 10)"
        )
    };
    format!(
        "PRECONDITION FAILED: {} keys did not split across regions within {:?} \
         ({rewrites} re-writes): {cause}. The gate's multi-region obligations are \
         VOID without splits.",
        wait.keys, wait.limit
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::future::pending;
    use std::future::ready;
    use std::future::Future;
    use std::future::Ready;
    use std::time::Duration;

    use tokio::time::sleep;
    use tokio::time::timeout;
    use tokio::time::Instant;

    use super::commit_limit_message;
    use super::wait_for_split;
    use super::SplitWait;

    /// How far past the limit a wait may end before the test calls it an overrun. Far
    /// above scheduling noise, and below the gap to what each test rules out.
    const MARGIN: Duration = Duration::from_secs(1);

    /// Every wait runs under this, so that a regression fails instead of hanging the run.
    const OUTER_LIMIT: Duration = Duration::from_secs(20);

    const POLL: Duration = Duration::from_millis(10);

    fn wait(limit: Duration, rewrite_every: Duration) -> SplitWait {
        SplitWait {
            keys: 200,
            limit,
            rewrite_every,
            poll: POLL,
        }
    }

    fn millis(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// A wait that never reaches a re-write.
    fn no_rewrites() -> Duration {
        Duration::from_secs(3600)
    }

    fn rewrite_not_expected() -> Ready<()> {
        panic!("this wait must not reach a re-write")
    }

    fn region_count_not_expected() -> Ready<u64> {
        panic!("a commit-limit failure must not call PD")
    }

    async fn bounded<T>(wait: impl Future<Output = T>) -> T {
        timeout(OUTER_LIMIT, wait)
            .await
            .expect("the wait must end by itself")
    }

    /// Slack on a reported running time: its rounding, and the gap between the wait's clock
    /// read and a stand-in's. Far below the gaps each test rules out.
    const CLOCK_SLACK: Duration = Duration::from_millis(20);

    fn commit_limit_text(commit: &str, running: &str, limit: &str, rewrites: u32) -> String {
        format!(
            "PRECONDITION FAILED: the {commit} commit had been running for {running}s and had not \
             returned when the {limit} limit for 200 keys to split across regions ran out \
             ({rewrites} re-writes). That leaves open whether this range can split: a commit that \
             ran for a long time points at TiKV or PD not answering it, while one cut off soon \
             after it started was only in flight. The gate's multi-region obligations are VOID \
             without splits."
        )
    }

    /// Asserts the whole commit-limit message, and returns the running time it reports. That
    /// figure comes from the real clock, so each test bounds it rather than spelling it.
    fn assert_commit_limit(message: &str, commit: &str, limit: &str, rewrites: u32) -> Duration {
        let (_, rest) = message
            .split_once("running for ")
            .unwrap_or_else(|| panic!("no running time: {message}"));
        let (running, _) = rest
            .split_once("s and")
            .unwrap_or_else(|| panic!("no running time: {message}"));
        assert_eq!(message, commit_limit_text(commit, running, limit, rewrites));
        assert!(!message.contains("cluster/tikv.toml"), "{message}");
        Duration::from_secs_f64(running.parse().expect("a number of seconds"))
    }

    /// Asserts that `running` is the time from `commit_started` to the limit, or to now if
    /// that is later, and not the time since the wait started.
    fn assert_ran_from(
        running: Duration,
        commit_started: Instant,
        wait_started: Instant,
        limit: Duration,
    ) {
        let to_limit = (wait_started + limit).saturating_duration_since(commit_started);
        assert!(
            running + CLOCK_SLACK >= to_limit,
            "reported {running:?}, but it ran until the limit, {to_limit:?} after it started"
        );
        assert!(
            running <= commit_started.elapsed() + CLOCK_SLACK,
            "reported {running:?}, but it started only {:?} ago",
            commit_started.elapsed()
        );
    }

    #[test]
    fn the_commit_limit_message_gives_the_running_time_to_the_millisecond() {
        let wait = wait(Duration::from_secs(90), Duration::from_secs(10));

        assert_eq!(
            commit_limit_message(&wait, "re-write", 8, millis(1234)),
            commit_limit_text("re-write", "1.234", "90s", 8)
        );
    }

    #[tokio::test]
    async fn a_seed_commit_that_never_returns_fails_at_the_limit() {
        let wait = wait(millis(200), no_rewrites());
        let probes = Cell::new(0);
        let start = Instant::now();

        let end = bounded(wait_for_split(
            &wait,
            5,
            pending(),
            rewrite_not_expected,
            || {
                probes.set(probes.get() + 1);
                ready(true)
            },
            region_count_not_expected,
        ))
        .await;

        let elapsed = start.elapsed();
        assert!(elapsed >= wait.limit, "ended early, after {elapsed:?}");
        assert!(elapsed < wait.limit + MARGIN, "overran, after {elapsed:?}");
        assert_eq!(probes.get(), 0, "no split probe after the commit limit");
        let message = end.expect_err("a stalled seed is a failure");
        let running = assert_commit_limit(&message, "seed", "200ms", 0);
        assert_ran_from(running, start, start, wait.limit);
    }

    /// The second re-write stalls at about 1.6 s of 2 s, so the time remaining ends the
    /// wait near 2 s; a limit of a full 2 s on that commit would end it near 3.6 s.
    #[tokio::test]
    async fn a_rewrite_commit_that_never_returns_fails_at_the_time_remaining() {
        let wait = wait(Duration::from_secs(2), millis(800));
        let calls = Cell::new(0);
        let probes = Cell::new(0);
        let probes_at_stall = Cell::new(None);
        let stalled_at = Cell::new(None);
        let start = Instant::now();

        let end = bounded(wait_for_split(
            &wait,
            5,
            ready(()),
            || {
                calls.set(calls.get() + 1);
                let stall = calls.get() == 2;
                if stall {
                    probes_at_stall.set(Some(probes.get()));
                    stalled_at.set(Some(Instant::now()));
                }
                async move {
                    if stall {
                        pending::<()>().await;
                    }
                }
            },
            || {
                probes.set(probes.get() + 1);
                ready(false)
            },
            region_count_not_expected,
        ))
        .await;

        let elapsed = start.elapsed();
        assert!(elapsed >= wait.limit, "ended early, after {elapsed:?}");
        assert!(elapsed < wait.limit + MARGIN, "overran, after {elapsed:?}");
        assert_eq!(calls.get(), 2);
        assert_eq!(
            probes_at_stall.get(),
            Some(probes.get()),
            "no split probe after the commit limit"
        );
        let message = end.expect_err("a stalled re-write is a failure");
        let running = assert_commit_limit(&message, "re-write", "2s", 1);
        let stalled_at = stalled_at.get().expect("it stalled");
        assert_ran_from(running, stalled_at, start, wait.limit);
    }

    #[tokio::test]
    async fn a_seed_commit_that_returns_late_is_not_a_pass() {
        let wait = wait(millis(200), no_rewrites());
        let returned = Cell::new(false);
        let start = Instant::now();

        let end = bounded(wait_for_split(
            &wait,
            5,
            async {
                sleep(millis(600)).await;
                returned.set(true);
            },
            rewrite_not_expected,
            || ready(returned.get()),
            region_count_not_expected,
        ))
        .await;

        assert!(!returned.get(), "the commit must be cancelled at the limit");
        let message = end.expect_err("a late seed is a failure");
        let running = assert_commit_limit(&message, "seed", "200ms", 0);
        assert_ran_from(running, start, start, wait.limit);
    }

    #[tokio::test]
    async fn a_rewrite_commit_that_returns_late_is_not_a_pass() {
        let wait = wait(millis(200), millis(50));
        let returned = Cell::new(false);
        let started_at = Cell::new(None);
        let start = Instant::now();

        let end = bounded(wait_for_split(
            &wait,
            5,
            ready(()),
            || async {
                started_at.set(Some(Instant::now()));
                sleep(millis(600)).await;
                returned.set(true);
            },
            || ready(returned.get()),
            region_count_not_expected,
        ))
        .await;

        assert!(!returned.get(), "the commit must be cancelled at the limit");
        let message = end.expect_err("a late re-write is a failure");
        let running = assert_commit_limit(&message, "re-write", "200ms", 0);
        let started_at = started_at.get().expect("it started");
        assert_ran_from(running, started_at, start, wait.limit);
    }

    /// A healthy commit can simply be under way when the limit arrives. The failure stands,
    /// but the message must say how little this one had run, so that it is not read as a
    /// stall. 500 ms from about 600 ms would end at 1.1 s of 1 s, cut off after about 400 ms;
    /// counted from the start of the wait it would read about 1 s.
    #[tokio::test]
    async fn a_short_rewrite_commit_that_crosses_the_limit_reports_how_little_it_ran() {
        let wait = wait(Duration::from_secs(1), millis(600));
        let returned = Cell::new(false);
        let started_at = Cell::new(None);
        let start = Instant::now();

        let end = bounded(wait_for_split(
            &wait,
            5,
            ready(()),
            || async {
                started_at.set(Some(Instant::now()));
                sleep(millis(500)).await;
                returned.set(true);
            },
            || ready(false),
            region_count_not_expected,
        ))
        .await;

        let elapsed = start.elapsed();
        assert!(elapsed >= wait.limit, "ended early, after {elapsed:?}");
        assert!(elapsed < wait.limit + MARGIN, "overran, after {elapsed:?}");
        assert!(!returned.get(), "the commit must be cancelled at the limit");
        let message = end.expect_err("a commit cut off at the limit is a failure");
        let running = assert_commit_limit(&message, "re-write", "1s", 0);
        let started_at = started_at.get().expect("it started");
        assert_ran_from(running, started_at, start, wait.limit);
    }

    /// A commit that holds the thread past the limit is ready at its first poll, so
    /// `timeout_at` reports it as returned in time. Only reading the clock catches it.
    #[tokio::test]
    async fn a_seed_commit_ready_only_after_the_limit_is_not_a_pass() {
        let wait = wait(millis(100), no_rewrites());
        let returned = Cell::new(false);
        let start = Instant::now();

        let end = bounded(wait_for_split(
            &wait,
            5,
            async {
                std::thread::sleep(millis(300));
                returned.set(true);
            },
            rewrite_not_expected,
            || ready(returned.get()),
            region_count_not_expected,
        ))
        .await;

        assert!(returned.get(), "the stand-in must have run to its end");
        let message = end.expect_err("a seed ready after the limit is a failure");
        let running = assert_commit_limit(&message, "seed", "100ms", 0);
        assert!(running + CLOCK_SLACK >= millis(300), "reported {running:?}");
        assert!(
            running <= start.elapsed() + CLOCK_SLACK,
            "reported {running:?}"
        );
    }

    #[tokio::test]
    async fn a_rewrite_commit_ready_only_after_the_limit_is_not_a_pass() {
        let wait = wait(millis(100), millis(10));
        let returned = Cell::new(false);
        let start = Instant::now();

        let end = bounded(wait_for_split(
            &wait,
            5,
            ready(()),
            || async {
                std::thread::sleep(millis(300));
                returned.set(true);
            },
            || ready(returned.get()),
            region_count_not_expected,
        ))
        .await;

        assert!(returned.get(), "the stand-in must have run to its end");
        let message = end.expect_err("a re-write ready after the limit is a failure");
        let running = assert_commit_limit(&message, "re-write", "100ms", 0);
        assert!(running + CLOCK_SLACK >= millis(300), "reported {running:?}");
        assert!(
            running <= start.elapsed() + CLOCK_SLACK,
            "reported {running:?}"
        );
    }

    /// A seed that takes 1.6 s of 2 s leaves 0.4 s for the wait. A deadline taken after
    /// the seed would end it near 3.6 s.
    #[tokio::test]
    async fn the_seed_commit_counts_against_the_limit() {
        let wait = wait(Duration::from_secs(2), no_rewrites());
        let start = Instant::now();

        let end = bounded(wait_for_split(
            &wait,
            5,
            sleep(millis(1600)),
            rewrite_not_expected,
            || ready(false),
            || ready(5),
        ))
        .await;

        let elapsed = start.elapsed();
        assert!(end.is_err(), "no split was reported: {end:?}");
        assert!(elapsed >= wait.limit, "ended early, after {elapsed:?}");
        assert!(elapsed < wait.limit + MARGIN, "overran, after {elapsed:?}");
    }

    #[tokio::test]
    async fn a_split_seen_at_the_first_probe_is_a_pass() {
        let wait = wait(Duration::from_secs(10), no_rewrites());

        let end = bounded(wait_for_split(
            &wait,
            5,
            ready(()),
            rewrite_not_expected,
            || ready(true),
            region_count_not_expected,
        ))
        .await;

        assert_eq!(end, Ok(0));
    }

    #[tokio::test]
    async fn a_split_seen_after_rewrites_is_a_pass_with_their_count() {
        let wait = wait(Duration::from_secs(10), millis(20));
        let rewrites = Cell::new(0);

        let end = bounded(wait_for_split(
            &wait,
            5,
            ready(()),
            || {
                rewrites.set(rewrites.get() + 1);
                ready(())
            },
            || ready(rewrites.get() == 3),
            region_count_not_expected,
        ))
        .await;

        assert_eq!(end, Ok(3));
    }

    /// Runs a wait in which commits return promptly and no split appears, and returns the
    /// message with the number of re-writes the commit stand-in saw.
    async fn no_split(regions_before: u64, regions_now: u64) -> (String, u32) {
        let wait = wait(millis(500), millis(100));
        let rewrites = Cell::new(Vec::new());
        let region_counts = Cell::new(0);
        let start = Instant::now();

        let end = bounded(wait_for_split(
            &wait,
            regions_before,
            ready(()),
            || {
                let mut seen = rewrites.take();
                seen.push(Instant::now());
                rewrites.set(seen);
                ready(())
            },
            || ready(false),
            || {
                region_counts.set(region_counts.get() + 1);
                ready(regions_now)
            },
        ))
        .await;

        let elapsed = start.elapsed();
        assert!(elapsed >= wait.limit, "ended early, after {elapsed:?}");
        assert!(elapsed < wait.limit + MARGIN, "overran, after {elapsed:?}");
        assert_eq!(region_counts.get(), 1, "one diagnostic region count");

        let seen = rewrites.take();
        assert!(seen.len() >= 2, "re-writes: {}", seen.len());
        let mut previous = start;
        for at in &seen {
            let gap = at.duration_since(previous);
            assert!(gap >= wait.rewrite_every, "re-written after only {gap:?}");
            previous = *at;
        }
        (
            end.expect_err("no split is a failure"),
            u32::try_from(seen.len()).expect("few re-writes"),
        )
    }

    #[tokio::test]
    async fn no_split_with_new_regions_keeps_the_rewrite_count_and_blames_the_checker_first() {
        let (message, rewrites) = no_split(5, 7).await;

        assert_eq!(
            message,
            format!(
                "PRECONDITION FAILED: 200 keys did not split across regions within 500ms \
                 ({rewrites} re-writes): the cluster has 7 regions (was 5), so regions are being \
                 created, but this range has not split yet. Most likely the split checker has not \
                 checked it even after the re-writes; if this repeats, check that \
                 cluster/tikv.toml is mounted (region-max-keys = 10). The gate's multi-region \
                 obligations are VOID without splits."
            )
        );
    }

    #[tokio::test]
    async fn no_split_without_new_regions_keeps_the_rewrite_count_and_blames_the_config() {
        let (message, rewrites) = no_split(5, 5).await;

        assert_eq!(
            message,
            format!(
                "PRECONDITION FAILED: 200 keys did not split across regions within 500ms \
                 ({rewrites} re-writes): no region has split at all (5 regions, was 5): check \
                 that cluster/tikv.toml is mounted (region-max-keys = 10). The gate's \
                 multi-region obligations are VOID without splits."
            )
        );
    }
}
