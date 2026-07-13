//! The client-rust parity driver: THE SUBJECT.
//!
//! Speaks the parity command protocol (newline-delimited JSON) over stdio, so the
//! runner can drive client-rust and client-go through the SAME scenario and diff what
//! they observed.
//!
//! This binary links `tikv-client` as a path dependency on the sibling checkout —
//! deliberately, because pointing the harness at a local branch to prove a fix is the
//! entire point of the repo. `scripts/provenance.sh` records which revision was
//! actually exercised, since Cargo cannot pin a path dep.
//!
//! FAULT INJECTION IS ABSENT ON PURPOSE. There is no `failpoints` feature here: this
//! binary's whole job is to observe an UNMODIFIED client, and `cargo build --workspace`
//! must never unify `fail/failpoints` into the tikv-client it links. (See
//! crates/wyrd-gate/Cargo.toml, where the feature is opt-in for exactly this reason.)

mod exec;
mod mapping;

use std::io::BufRead;
use std::io::Write;
use std::os::fd::FromRawFd;

use parity_proto::Command;

fn main() {
    // ── STDOUT HYGIENE ───────────────────────────────────────────────────────
    // The protocol owns fd 1, and NOTHING else may write to it. `tikv-client` logs
    // via the `log` crate, and any dependency can install a logger that writes to
    // stdout; one stray line corrupts the NDJSON stream and the run fails as a parse
    // error far from the cause.
    //
    // So: duplicate fd 1 for ourselves, then point the REAL fd 1 at stderr. Stray
    // output still goes somewhere a human can read it; it just cannot reach the
    // protocol. Linux-only, as is this whole repo (cluster/docker-compose.yml uses
    // network_mode: host).
    //
    // SAFETY: dup/dup2 on fds we own, once, before any thread is spawned.
    let proto_fd = unsafe { libc::dup(1) };
    assert!(proto_fd >= 0, "cannot dup stdout");
    assert!(
        unsafe { libc::dup2(2, 1) } >= 0,
        "cannot redirect stdout to stderr"
    );
    let mut proto_out = unsafe { std::fs::File::from_raw_fd(proto_fd) };

    // Logs go to stderr, and are captured into the trace's evidence on failure.
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Stderr)
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut driver = exec::Driver::new();
    let stdin = std::io::stdin();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            Ok(_) => continue,
            Err(e) => {
                eprintln!("driver: stdin: {e}");
                std::process::exit(1);
            }
        };

        let response = match serde_json::from_str::<Command>(&line) {
            Ok(cmd) => rt.block_on(driver.execute(cmd)),
            // A malformed command is a HARNESS failure, not a client observation. It
            // must be reported as driver_error (inadmissible), never as some
            // plausible-looking client behaviour.
            Err(e) => exec::Response::observation(parity_proto::Observation::driver_error(
                format!("malformed command: {e}"),
            )),
        };

        let encoded = serde_json::to_string(&response).expect("a response must serialize");
        writeln!(proto_out, "{encoded}").expect("cannot write response");
        proto_out.flush().expect("cannot flush response");
    }
}
