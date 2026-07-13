// The client-go parity driver: THE ORACLE.
//
// Speaks the parity command protocol (newline-delimited JSON, request -> response)
// over stdio, so the runner can drive client-go and client-rust through the SAME
// scenario and diff what they observed.
//
// This binary links client-go at the PINNED pseudo-version, with no `replace` — see
// pins.toml: "an oracle you can accidentally edit is not an oracle." `hello` reports
// the version it actually linked, and whether the module was replaced, so
// ledger-check can refuse a result produced against an oracle someone can edit. That
// check is made from inside the binary that ran, not from a file describing it.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"runtime/debug"
	"syscall"
)

const (
	protocolVersion = "parity-cmd/v1"
	clientGoModule  = "github.com/tikv/client-go/v2"
)

func main() {
	// ── STDOUT HYGIENE ───────────────────────────────────────────────────────
	// The protocol owns fd 1, and NOTHING else may write to it. client-go logs via
	// zap, and any dependency can reconfigure a logger to stdout; a single stray
	// line corrupts the NDJSON stream mid-scenario and the run fails as a parse
	// error somewhere far from the cause.
	//
	// So: duplicate fd 1 for ourselves, then point the REAL fd 1 at stderr. Stray
	// output still goes somewhere a human can read; it just cannot reach the
	// protocol. (Linux-only — as is this whole repo: cluster/docker-compose.yml
	// uses network_mode: host.)
	protoFd, err := syscall.Dup(1)
	if err != nil {
		fmt.Fprintf(os.Stderr, "driver: cannot dup stdout: %v\n", err)
		os.Exit(1)
	}
	if err := syscall.Dup2(2, 1); err != nil {
		fmt.Fprintf(os.Stderr, "driver: cannot redirect stdout to stderr: %v\n", err)
		os.Exit(1)
	}
	protoOut := os.NewFile(uintptr(protoFd), "protocol")
	defer protoOut.Close()

	d := NewDriver()
	defer d.Close()

	enc := json.NewEncoder(protoOut)
	in := bufio.NewScanner(os.Stdin)
	// Scenarios can carry large values; the default 64KiB token cap is a trap that
	// would surface as a mysterious truncation rather than an error.
	in.Buffer(make([]byte, 0, 1<<20), 16<<20)

	for in.Scan() {
		line := in.Bytes()
		if len(line) == 0 {
			continue
		}

		var cmd Command
		if err := json.Unmarshal(line, &cmd); err != nil {
			// A malformed command is a HARNESS failure, not a client observation.
			// It must be reported as driver_error (inadmissible), never as some
			// plausible-looking client behaviour.
			_ = enc.Encode(Response{
				Observation: driverError(fmt.Sprintf("malformed command: %v", err)),
			})
			continue
		}

		resp := d.Execute(cmd)
		if err := enc.Encode(resp); err != nil {
			fmt.Fprintf(os.Stderr, "driver: cannot write response: %v\n", err)
			os.Exit(1)
		}
	}
	if err := in.Err(); err != nil {
		fmt.Fprintf(os.Stderr, "driver: stdin: %v\n", err)
		os.Exit(1)
	}
}

// Response is one reply. Exactly one field is set.
type Response struct {
	Hello       *Hello       `json:"hello,omitempty"`
	Observation *Observation `json:"observation,omitempty"`
}

// Hello identifies this driver and — critically — the client it ACTUALLY linked.
type Hello struct {
	Driver   string   `json:"driver"`
	Protocol string   `json:"protocol"`
	Client   ClientID `json:"client"`
	Features []string `json:"features"`
	Config   []string `json:"config"`
}

type ClientID struct {
	Name string `json:"name"`
	// Read from the BUILD, never from pins.toml. A driver that reported the version
	// it was told to expect would be a mirror, not a witness.
	Version string `json:"version"`
	// Was client-go swapped for a local tree (a stray go.work, a `replace`)? If so,
	// the oracle is editable and every claim settled against it is void.
	Replaced bool `json:"replaced"`
}

// buildInfo reports the client-go version this binary actually linked.
func buildInfo() ClientID {
	id := ClientID{Name: clientGoModule, Version: "unknown"}
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		return id
	}
	for _, dep := range bi.Deps {
		if dep.Path != clientGoModule {
			continue
		}
		id.Version = dep.Version
		if dep.Replace != nil {
			// THE CHECK THAT MATTERS. pins.toml calls a replaced oracle "not an
			// oracle"; until now that was a comment. Report it, and let ledger-check
			// refuse the run.
			id.Replaced = true
			id.Version = dep.Replace.Version
			if dep.Replace.Path != dep.Path {
				id.Version = fmt.Sprintf("REPLACED=>%s@%s", dep.Replace.Path, dep.Replace.Version)
			}
		}
		return id
	}
	return id
}

func hello() *Hello {
	return &Hello{
		Driver:   "go",
		Protocol: protocolVersion,
		Client:   buildInfo(),
		// prewrite_only is THE capability that makes this driver the state factory:
		// client-go exports CommitterProbe from a non-test file, so an external
		// module can drive 2PC one phase at a time. client-rust has no equivalent.
		Features: []string{"prewrite_only", "scan_locks"},
		// DECLARE EVERY DEVIATION FROM A STOCK CLIENT. It lands in the trace, and
		// from there in the evidence — rather than in a code comment nobody reads six
		// months from now, when the result is being quoted and someone asks what
		// exactly was running. See disableRouterClient() for the full reasoning.
		Config: []string{
			"pd.enable_router_client=false (PD v8.5.5 does not implement QueryRegion; " +
				"falls back to the classic GetRegion RPC, which is the path client-rust uses)",
		},
	}
}
