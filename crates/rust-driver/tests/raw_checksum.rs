//! The Rust driver's `raw_checksum` arm, cluster-free: what `hello` advertises, and how
//! a client that was never opened is answered.
//!
//! Drives the built binary over stdin/stdout the way the runner does, and nothing else:
//! no fake PD or TiKV, no stand-in for the client. An unknown raw client is answered
//! before any connection is attempted, so the arm's dispatch is reachable without a
//! cluster; the real `RawClient::checksum` call is shown live by the parity ledger
//! (G-0002, `scenarios/raw-checksum.json`).
//!
//! Only the `hello` assertion depends on the build, and it checks the BUILD, not what the
//! linked client can do: without `pre-raw-checksum` (every gating build) the driver lists
//! `raw_checksum`; with it (CI's upstream view) it does not, whatever client-rust it
//! links. The unknown-client answer is the same in both builds — a `driver_error` naming
//! the client, never `unsupported` (`docs/design/specs/parity-protocol/v1.md` §4.3
//! rule 4).

use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::process::Command;
use std::process::Stdio;

/// Send `commands` to a fresh driver, one per line, close its stdin so it exits on EOF,
/// and return every reply it wrote, parsed.
fn drive(commands: &[&str]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_parity-driver-rust"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the driver");
    {
        let mut stdin = child.stdin.take().expect("driver stdin");
        for c in commands {
            writeln!(stdin, "{c}").expect("write a command");
        }
        // Dropping stdin closes it: the driver's loop ends on EOF.
    }
    let stdout = child.stdout.take().expect("driver stdout");
    let replies: Vec<serde_json::Value> = BufReader::new(stdout)
        .lines()
        .map(|l| l.expect("read a reply"))
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(&l).unwrap_or_else(|e| panic!("reply {l:?}: {e}")))
        .collect();
    let status = child.wait().expect("wait for the driver");
    assert!(status.success(), "the driver exited with {status}");
    assert_eq!(
        replies.len(),
        commands.len(),
        "one reply per command: {replies:?}"
    );
    replies
}

#[test]
fn hello_lists_raw_checksum_unless_built_with_pre_raw_checksum() {
    let replies = drive(&[r#"{"op":"hello"}"#]);
    let features = replies[0]["hello"]["features"]
        .as_array()
        .unwrap_or_else(|| panic!("hello.features is a list: {:?}", replies[0]));
    let listed = features.iter().any(|f| f == "raw_checksum");
    if cfg!(feature = "pre-raw-checksum") {
        assert!(
            !listed,
            "built with pre-raw-checksum (the upstream view), hello must not list \
             raw_checksum: {features:?}"
        );
    } else {
        assert!(
            listed,
            "built without pre-raw-checksum, hello must list raw_checksum as the Go \
             driver does: {features:?}"
        );
    }
}

#[test]
fn raw_checksum_on_a_never_opened_client_is_a_driver_error_naming_it() {
    let replies = drive(&[
        r#"{"op":"hello"}"#,
        r#"{"op":"raw_checksum","client":"never-opened","start":{"s":"a"},"end":{"s":"b"}}"#,
    ]);
    let obs = &replies[1]["observation"];
    assert_eq!(
        obs["class"], "driver_error",
        "an unknown raw client is a harness fault, never a client observation (and never \
         `unsupported`): {obs}"
    );
    let detail = obs["detail"]
        .as_str()
        .unwrap_or_else(|| panic!("driver_error carries a detail string: {obs}"));
    assert!(
        detail.contains("never-opened"),
        "the detail names the unknown client, as raw_put's does: {detail:?}"
    );
}
