// Command dispatch for the client-go driver.
package main

import (
	"context"
	"encoding/base64"
	"fmt"
	"os"
	"strings"

	"github.com/tikv/client-go/v2/oracle"
	"github.com/tikv/client-go/v2/tikv"
	"github.com/tikv/client-go/v2/txnkv"
	"github.com/tikv/client-go/v2/txnkv/transaction"
	"github.com/tikv/pd/client/opt"
)

// Command mirrors parity-proto's Rust enum (serde tag = "op", snake_case).
type Command struct {
	Op string `json:"op"`

	Name    string `json:"name,omitempty"`
	Session string `json:"session,omitempty"`
	Client  string `json:"client,omitempty"`
	Mode    string `json:"mode,omitempty"`

	Key     *KeyArg  `json:"key,omitempty"`
	Value   *KeyArg  `json:"value,omitempty"`
	Primary *KeyArg  `json:"primary,omitempty"`
	Keys    []KeyArg `json:"keys,omitempty"`

	Start *KeyArg `json:"start,omitempty"`
	End   *KeyArg `json:"end,omitempty"`
	Limit uint32  `json:"limit,omitempty"`
}

// KeyArg is {"s": "utf8"} or {"b64": "..."}.
type KeyArg struct {
	S   *string `json:"s,omitempty"`
	B64 *string `json:"b64,omitempty"`
}

func (k *KeyArg) Bytes() []byte {
	if k == nil {
		return nil
	}
	if k.S != nil {
		return []byte(*k.S)
	}
	if k.B64 != nil {
		raw, err := base64.StdEncoding.DecodeString(*k.B64)
		if err != nil {
			return nil
		}
		return raw
	}
	return nil
}

// Driver holds the sessions. One process per ROLE, never per language — client-go's
// config is a process global (config.GetGlobalConfig), so two differently-configured
// Go clients cannot cleanly coexist in one process. Making the process boundary the
// role boundary sidesteps that entirely.
type Driver struct {
	clients map[string]*txnkv.Client
	txns    map[string]*transaction.KVTxn
	// The committer a prewrite_only left mid-2PC, so `abandon` can drop it without
	// cleanup — that residue IS the state under test.
	committers map[string]*transaction.CommitterProbe
}

func NewDriver() *Driver {
	return &Driver{
		clients:    map[string]*txnkv.Client{},
		txns:       map[string]*transaction.KVTxn{},
		committers: map[string]*transaction.CommitterProbe{},
	}
}

func (d *Driver) Close() {
	for _, c := range d.clients {
		_ = c.Close()
	}
}

func pdAddrs() []string {
	v := os.Getenv("PD_ADDRS")
	if v == "" {
		v = "127.0.0.1:2379"
	}
	return strings.Split(v, ",")
}

// disableRouterClient turns OFF the PD "router client", and this is a DECLARED
// deviation from a stock client-go — see hello.config, which puts it in every trace.
//
// WHY IT IS NECESSARY. The pinned client-go (pins.toml client_go) depends on a PD
// client that, by default, resolves regions through the streaming `QueryRegion` RPC.
// The pinned cluster is PD v8.5.5 (pins.toml cluster — chosen because it is
// client-rust's own CI pin), and v8.5.5 does not implement that method:
//
//	rpc error: code = Unimplemented desc = unknown method QueryRegion for service pdpb.PD
//
// Left on, every region lookup fails and the driver hangs retrying forever. So the two
// pins are genuinely incompatible on this one PD API, and something has to give.
//
// WHY IT IS THE RIGHT THING TO GIVE. The router client is a region-lookup TRANSPORT
// optimization, not TiKV transaction semantics. Disabling it makes client-go resolve
// regions through the classic `GetRegion` RPC — which is the very path client-rust
// uses. For a harness whose whole purpose is to compare the two clients' behaviour,
// having them share a region-lookup path is more faithful, not less: it removes a
// difference that is about PD versions rather than about either client.
//
// WHY NOT JUST BUMP THE CLUSTER. Because the cluster pin is load-bearing elsewhere:
// the wyrd-gate's entire verdict is stated against v8.5.5, and bumping it would
// re-state every one of those claims. That is a deliberate, reviewed change, not
// something to do in passing to make a new driver start up.
//
// The honest cost: any gap that lives specifically IN the router path is invisible to
// this harness. That is a real limitation and it belongs in the ledger's scope notes,
// not buried here.
func disableRouterClient(cli *txnkv.Client) error {
	if err := cli.GetPDClient().UpdateOption(opt.EnableRouterClient, false); err != nil {
		return fmt.Errorf("cannot disable the PD router client: %w", err)
	}
	return nil
}

func (d *Driver) Execute(cmd Command) Response {
	ctx := context.Background()

	switch cmd.Op {
	case "hello":
		return Response{Hello: hello()}

	case "open_client":
		cli, err := txnkv.NewClient(pdAddrs())
		if err != nil {
			return Response{Observation: driverError(fmt.Sprintf("open_client: %v", err))}
		}
		if err := disableRouterClient(cli); err != nil {
			return Response{Observation: driverError(fmt.Sprintf("open_client: %v", err))}
		}
		d.clients[cmd.Name] = cli
		return Response{Observation: ok()}

	case "close_client":
		if cli, found := d.clients[cmd.Name]; found {
			_ = cli.Close()
			delete(d.clients, cmd.Name)
		}
		return Response{Observation: ok()}

	case "begin":
		cli, found := d.clients[cmd.Client]
		if !found {
			return Response{Observation: driverError("begin: no such client " + cmd.Client)}
		}
		txn, err := cli.Begin()
		if err != nil {
			return Response{Observation: driverError(fmt.Sprintf("begin: %v", err))}
		}
		// Go's default is optimistic; Rust's TransactionOptions::default() is
		// PESSIMISTIC. The scenario always states the mode explicitly so that
		// difference can never leak in as an unexamined default.
		if cmd.Mode == "pessimistic" {
			txn.SetPessimistic(true)
		}
		d.txns[cmd.Session] = txn
		return Response{Observation: ok().withStartTS(txn.StartTS())}

	case "put":
		txn, resp := d.txn(cmd.Session)
		if txn == nil {
			return resp
		}
		if err := txn.Set(cmd.Key.Bytes(), cmd.Value.Bytes()); err != nil {
			return Response{Observation: classify(err)}
		}
		return Response{Observation: ok()}

	case "get":
		txn, resp := d.txn(cmd.Session)
		if txn == nil {
			return resp
		}
		entry, err := txn.Get(ctx, cmd.Key.Bytes())
		if err != nil {
			return Response{Observation: classify(err)}
		}
		return Response{Observation: ok().withValue(entry.Value)}

	case "commit":
		txn, resp := d.txn(cmd.Session)
		if txn == nil {
			return resp
		}
		err := txn.Commit(ctx)
		delete(d.txns, cmd.Session)
		if err != nil {
			return Response{Observation: classify(err)}
		}
		return Response{Observation: ok().withCommitTS(txn.CommitTS())}

	case "rollback":
		txn, resp := d.txn(cmd.Session)
		if txn == nil {
			return resp
		}
		err := txn.Rollback()
		delete(d.txns, cmd.Session)
		if err != nil {
			return Response{Observation: classify(err)}
		}
		return Response{Observation: ok()}

	case "snapshot_get":
		// A read OUTSIDE any transaction, at a fresh timestamp. This is the parity
		// claim in the orphaned-lock scenario: "is the key readable again?"
		cli, found := d.clients[cmd.Client]
		if !found {
			return Response{Observation: driverError("snapshot_get: no such client " + cmd.Client)}
		}
		ts, err := cli.CurrentTimestamp(oracle.GlobalTxnScope)
		if err != nil {
			return Response{Observation: driverError(fmt.Sprintf("snapshot_get: ts: %v", err))}
		}
		snap := cli.GetSnapshot(ts)
		entry, err := snap.Get(ctx, cmd.Key.Bytes())
		if err != nil {
			return Response{Observation: classify(err)}
		}
		return Response{Observation: ok().withValue(entry.Value)}

	case "scan_locks":
		// Ground truth for durable lock residue, and symmetric with the Rust
		// driver's TransactionClient::scan_locks. This is what makes findings 1 and
		// 2 DIFFABLE rather than merely assertable.
		cli, found := d.clients[cmd.Client]
		if !found {
			return Response{Observation: driverError("scan_locks: no such client " + cmd.Client)}
		}
		ts, err := cli.CurrentTimestamp(oracle.GlobalTxnScope)
		if err != nil {
			return Response{Observation: driverError(fmt.Sprintf("scan_locks: ts: %v", err))}
		}
		probe := tikv.StoreProbe{KVStore: cli.KVStore}
		locks, err := probe.ScanLocks(ctx, cmd.Start.Bytes(), cmd.End.Bytes(), ts)
		if err != nil {
			return Response{Observation: classify(err)}
		}
		return Response{Observation: ok().withLocks(mkLocks(locks))}

	case "prewrite_only":
		return Response{Observation: d.prewriteOnly(ctx, cmd)}

	case "abandon":
		// Drop the txn WITHOUT commit or rollback — "the crash". The locks a
		// prewrite left behind stay behind, which is the entire point.
		delete(d.txns, cmd.Session)
		delete(d.committers, cmd.Session)
		return Response{Observation: ok()}

	default:
		return Response{Observation: driverError("unknown op: " + cmd.Op)}
	}
}

func (d *Driver) txn(session string) (*transaction.KVTxn, Response) {
	txn, found := d.txns[session]
	if !found {
		return nil, Response{Observation: driverError("no such session: " + session)}
	}
	return txn, Response{}
}

func (o *Observation) withValue(v []byte) *Observation {
	b := mkBytes(v)
	o.Value = &b
	return o
}

func (o *Observation) withLocks(l []Lock) *Observation {
	o.Locks = l
	return o
}

func (o *Observation) withStartTS(ts uint64) *Observation {
	o.StartTS = ts
	return o
}

func (o *Observation) withCommitTS(ts uint64) *Observation {
	o.CommitTS = ts
	return o
}
