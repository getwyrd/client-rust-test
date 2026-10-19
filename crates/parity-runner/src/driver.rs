//! A driver child process, and the NDJSON conversation with it.
//!
//! ONE PROCESS PER ROLE, never per language. This is forced, not stylistic: client-go's
//! config is a process global (`config.GetGlobalConfig`), so two differently-configured
//! Go clients cannot cleanly coexist in one process. Making the process boundary the
//! ROLE boundary sidesteps that entirely — and as a bonus it isolates client-rust's
//! process-global `fail` registry too.

use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::process::Child;
use std::process::ChildStdin;
use std::process::ChildStdout;
use std::process::Command as OsCommand;
use std::process::Stdio;

use parity_proto::Command;
use parity_proto::Hello;
use parity_proto::Observation;
use serde::Deserialize;

/// A reply from a driver. Exactly one field is set.
#[derive(Debug, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub hello: Option<Hello>,
    #[serde(default)]
    pub observation: Option<Observation>,
}

pub struct Driver {
    pub name: String,
    pub hello: Hello,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Driver {
    /// Spawn a driver and complete the handshake.
    pub fn spawn(name: &str, binary: &std::path::Path) -> Result<Self, String> {
        // stderr is INHERITED, deliberately: both drivers redirect their own stray
        // stdout to stderr (a stray log line would otherwise corrupt the NDJSON
        // stream), and when a run goes wrong those logs are the first thing a human
        // needs. Swallowing them would make every failure a mystery.
        let mut child = driver_command(binary, std::env::var_os("PARITY_DRIVER_WRAPPER"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("cannot spawn {} driver ({}): {e}", name, binary.display()))?;

        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));

        let mut d = Driver {
            name: name.to_owned(),
            hello: Hello {
                driver: String::new(),
                protocol: String::new(),
                client: parity_proto::command::ClientId {
                    name: String::new(),
                    version: String::new(),
                    replaced: false,
                },
                features: vec![],
                config: vec![],
            },
            child,
            stdin,
            stdout,
        };

        let resp = d.send(&Command::Hello)?;
        let hello = resp
            .hello
            .ok_or_else(|| format!("{name}: driver did not answer `hello` with a hello"))?;

        // A one-sided protocol bump must be a HARD failure, not a silent mismatch that
        // shows up later as a mysterious field disagreement. The two halves of the
        // schema are hand-written in different languages; this is the cheap check that
        // they still agree on which schema they are writing.
        if hello.protocol != parity_proto::PROTOCOL_VERSION {
            return Err(format!(
                "{name}: driver speaks protocol `{}`, runner speaks `{}`. \
                 The schema's two halves have drifted.",
                hello.protocol,
                parity_proto::PROTOCOL_VERSION
            ));
        }
        d.hello = hello;
        Ok(d)
    }

    /// Send one command; read one response.
    pub fn send(&mut self, cmd: &Command) -> Result<Response, String> {
        let line = serde_json::to_string(cmd).map_err(|e| format!("encode: {e}"))?;
        writeln!(self.stdin, "{line}").map_err(|e| format!("{}: write: {e}", self.name))?;
        self.stdin
            .flush()
            .map_err(|e| format!("{}: flush: {e}", self.name))?;

        let mut reply = String::new();
        let n = self
            .stdout
            .read_line(&mut reply)
            .map_err(|e| format!("{}: read: {e}", self.name))?;
        if n == 0 {
            return Err(format!(
                "{}: driver closed its stream (it probably crashed — see its stderr above)",
                self.name
            ));
        }
        serde_json::from_str(&reply)
            .map_err(|e| format!("{}: cannot parse reply `{}`: {e}", self.name, reply.trim()))
    }
}

/// The command that starts a driver: the binary itself, or `<wrapper> <binary>` when
/// `PARITY_DRIVER_WRAPPER` names a wrapper.
///
/// The wrapper exists for the pdca loop's T3 gate, which judges a CHANGE to client-rust
/// or to a driver. The drivers are where that change's code runs, so they must not share
/// the runner's sandbox: there they could write over results/ or the scripts that judge
/// them. The gate's wrapper starts each driver in a sandbox of its own and connects its
/// stdin and stdout back here; the drivers need nothing else, since the conversation is
/// NDJSON over stdio. Unset (CI, a human's `make parity`), nothing changes.
fn driver_command(binary: &std::path::Path, wrapper: Option<std::ffi::OsString>) -> OsCommand {
    match wrapper {
        Some(w) if !w.is_empty() => {
            let mut c = OsCommand::new(w);
            c.arg(binary);
            c
        }
        _ => OsCommand::new(binary),
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_wrapper_the_driver_runs_itself() {
        let bin = std::path::Path::new("target/debug/parity-driver-rust");
        for w in [None, Some(std::ffi::OsString::new())] {
            let c = driver_command(bin, w);
            assert_eq!(c.get_program(), bin.as_os_str());
            assert_eq!(c.get_args().count(), 0);
        }
    }

    #[test]
    fn a_wrapper_is_given_the_driver_binary() {
        let bin = std::path::Path::new("target/debug/parity-driver-rust");
        let c = driver_command(bin, Some("/opt/wrap".into()));
        assert_eq!(c.get_program(), "/opt/wrap");
        assert_eq!(c.get_args().collect::<Vec<_>>(), [bin.as_os_str()]);
    }
}
