// THE GO HALF OF THE MAPPING TABLE.
//
// Its Rust twin is crates/rust-driver/src/mapping.rs. Between them they define what
// it MEANS for two clients to have done the same thing, so this is the one file where
// a lie could hide — and the four defenses against that are:
//
//  1. Nothing is normalized at capture. `class` is a projection of what the SERVER
//     said; the client's own error is carried verbatim in `native` and the kvproto in
//     `proto`. A mapping call can always be overturned from the evidence.
//  2. `native` is ALWAYS attached. If this table maps two different facts to one
//     class, the trace still contains the difference and a ledger claim can opt in to
//     diffing `native.type`. (Finding 1 lives exactly there.)
//  3. Every arm below cites the client-go source it mirrors, by file and line.
//  4. The catch-all is `internal`, which is INADMISSIBLE. An unmapped error can never
//     become `ok` or `write_conflict`; it becomes a run ledger-check refuses. A result
//     that MIGHT be a lie is worth less than no result at all.
//
// THE RULE: normalize presentation, never fact. Go returning ErrNotExist where Rust
// returns Ok(None) is one fact spelled two ways -> both `not_found`. Rust failing to
// resolve a lock where Go succeeds is TWO FACTS, and must stay two.
package main

import (
	"encoding/base64"
	"errors"
	"fmt"
	"regexp"
	"strings"
	"unicode"

	"github.com/pingcap/kvproto/pkg/kvrpcpb"
	tikverr "github.com/tikv/client-go/v2/error"
	"github.com/tikv/client-go/v2/txnkv/txnlock"
)

// Observation is one command's result. Mirrors parity-proto's Rust type exactly —
// by hand: there is no generated binding and no golden corpus (yet), so the two
// halves are kept in lockstep by review. A field added on one side without the other
// serializes as absent and the projection treats absent as a fact, so drift here is
// not cosmetic.
type Observation struct {
	Class string `json:"class"`

	// Set only for the classes that carry one (unsupported / driver_error / internal).
	Detail string `json:"detail,omitempty"`
	// Set only for `mixed`.
	Parts []string `json:"parts,omitempty"`

	Value *Bytes `json:"value,omitempty"`
	Locks []Lock `json:"locks,omitempty"`

	StartTS  uint64 `json:"start_ts,omitempty"`
	CommitTS uint64 `json:"commit_ts,omitempty"`

	// How many per-key errors this client surfaced. For client-go the answer is always
	// ONE: ExtractKeyErr (error/error.go:331) returns a single error even when several
	// keys failed. client-rust surfaces all of them.
	//
	// It must be emitted, not left absent, or a claim opting into `errors.count` would
	// compare Go's `<absent>` against Rust's "1" and diverge even when both clients
	// surfaced exactly one error — a false divergence manufactured by an unset field. And
	// "client-go collapses N errors into 1" is a real, statable parity fact; leaving the
	// field empty would make it indistinguishable from "we did not look".
	ErrorCount *int `json:"error_count,omitempty"`

	Proto  map[string]any `json:"proto,omitempty"`
	Native *Native        `json:"native,omitempty"`

	// The result of a raw checksum, when this client could compute one. EVIDENCE
	// ONLY, never projected: crc64_xor/total_bytes cover the key bytes — run prefix
	// included — so they legitimately differ between the two runs of one comparison.
	Checksum *ChecksumObs `json:"checksum,omitempty"`
}

// ChecksumObs mirrors parity_proto::ChecksumObs (kvrpcpb.RawChecksumResponse, verbatim).
type ChecksumObs struct {
	Crc64Xor   uint64 `json:"crc64_xor"`
	TotalKvs   uint64 `json:"total_kvs"`
	TotalBytes uint64 `json:"total_bytes"`
}

type Bytes struct {
	B64  string  `json:"b64"`
	UTF8 *string `json:"utf8,omitempty"`
}

type Lock struct {
	Key        Bytes  `json:"key"`
	Primary    Bytes  `json:"primary"`
	Kind       string `json:"kind"`
	TTLms      uint64 `json:"ttl_ms"`
	TxnStartTS uint64 `json:"txn_start_ts"`
}

type Native struct {
	Lang    string `json:"lang"`
	Type    string `json:"type"`
	Display string `json:"display"`
}

func mkBytes(raw []byte) Bytes {
	b := Bytes{B64: base64.StdEncoding.EncodeToString(raw)}
	if s := string(raw); isPrintable(s) {
		b.UTF8 = &s
	}
	return b
}

func isPrintable(s string) bool {
	for _, r := range s {
		if r == unicode.ReplacementChar || unicode.IsControl(r) {
			return false
		}
	}
	return true
}

func ok() *Observation                  { return &Observation{Class: "ok"} }
func notFound() *Observation            { return &Observation{Class: "not_found"} }
func unsupported(d string) *Observation { return &Observation{Class: "unsupported", Detail: d} }
func driverError(d string) *Observation { return &Observation{Class: "driver_error", Detail: d} }

// lockKind renders kvrpcpb.Op BY NAME. Both clients generate this enum from the same
// proto, so the name is exact — and a name survives a renumbering that a bare integer
// would not.
// The mapping must be TOTAL over kvrpcpb.Op, and it must agree with the Rust driver's
// `lock_kind` arm for arm. Both clients generate this enum from the same proto, so any
// operation one of them names and the other renders as a bare `op_N` is a divergence the
// HARNESS invented — the clients returned the identical protobuf value.
//
// Op_Rollback was exactly that: Rust said "rollback", Go said "op_3".
func lockKind(op kvrpcpb.Op) string {
	switch op {
	case kvrpcpb.Op_Put:
		return "put"
	case kvrpcpb.Op_Del:
		return "del"
	case kvrpcpb.Op_Lock:
		return "lock"
	case kvrpcpb.Op_Rollback:
		return "rollback"
	case kvrpcpb.Op_Insert:
		return "insert"
	case kvrpcpb.Op_PessimisticLock:
		return "pessimistic_lock"
	case kvrpcpb.Op_CheckNotExists:
		return "check_not_exists"
	default:
		// Deliberately still reachable: if the proto grows an operation, BOTH drivers
		// render it as `op_N` and agree, rather than one of them guessing at a name.
		return fmt.Sprintf("op_%d", int32(op))
	}
}

func mkLocks(locks []*txnlock.Lock) []Lock {
	out := make([]Lock, 0, len(locks))
	for _, l := range locks {
		out = append(out, Lock{
			Key:        mkBytes(l.Key),
			Primary:    mkBytes(l.Primary),
			Kind:       lockKind(l.LockType),
			TTLms:      l.TTL,
			TxnStartTS: l.TxnID,
		})
	}
	return out
}

// txnNotFoundRe matches client-go's UNTYPED TxnNotFound.
//
// THE ONE UGLY ARM, and it is unavoidable. Every other KeyError becomes a typed error
// in ExtractKeyErr, but TxnNotFound does not:
//
//	client-go error/error.go:363
//	    if keyErr.TxnNotFound != nil {
//	        err := errors.Errorf("txn %d not found", keyErr.TxnNotFound.StartTs)
//	        return err
//	    }
//
// There is no typed error and no wrapped proto to match on, so the only handle is the
// message text. That is fragile, and it is precisely why it is confined to ONE place
// with this comment: if client-go ever rewords it, this stops matching and the arm
// falls through to `internal` — INADMISSIBLE, a loud failure, not a silent
// misclassification. The failure mode is correct even when the match is wrong.
//
// (This asymmetry is itself worth noting upstream: TxnNotFound is the one KeyError
// client-go leaves untyped, and it happens to be the one finding 1 turns on.)
var txnNotFoundRe = regexp.MustCompile(`^txn \d+ not found`)

// classify maps a client-go error onto the canonical vocabulary.
//
// Order matters: the typed errors first, the string-matched wart last, and an explicit
// `internal` for anything unrecognized.
func classify(err error) *Observation {
	if err == nil {
		return ok()
	}

	// ── not found: a NORMAL OUTCOME, not an error ────────────────────────────
	// Checked FIRST, and deliberately WITHOUT a `native` block.
	//
	// client-go signals "the key is absent" with a sentinel error (error/error.go:60,
	// ErrNotExist); client-rust signals it with Ok(None). Same fact, different calling
	// convention. If we attached Go's native error type here, then EVERY not_found
	// comparison would carry `native.type = *errors.fundamental` on the Go side and
	// `<absent>` on the Rust side, and any claim that opts into diffing native.type
	// would see a PERMANENT divergence that is not a gap at all — it is a fact about
	// Go's calling convention, not about TiKV.
	//
	// This is the boundary the mapping rule draws: normalize PRESENTATION (how a
	// language spells "absent"), never FACT. `native` exists to preserve a client's
	// taxonomy OF AN ERROR; a successful absent-key read has no error to have a
	// taxonomy of.
	if tikverr.IsErrNotFound(err) {
		return notFound()
	}

	// client-go surfaces exactly one error, however many keys failed: ExtractKeyErr
	// returns a single error. Stating that explicitly is what lets a claim compare
	// cardinality against client-rust, which surfaces all of them.
	one := 1
	obs := &Observation{
		ErrorCount: &one,
		Native: &Native{
			Lang:    "go",
			Type:    fmt.Sprintf("%T", errors.Unwrap(err)),
			Display: err.Error(),
		},
	}
	// %T of the unwrapped error is often more informative, but can be nil for a
	// leaf error; fall back to the error's own type.
	if obs.Native.Type == "<nil>" {
		obs.Native.Type = fmt.Sprintf("%T", err)
	}

	// ── write conflict — the lost-race signal ────────────────────────────────
	// client-go error/error.go:166. Carries the kvrpcpb.WriteConflict proto.
	var wc *tikverr.ErrWriteConflict
	if errors.As(err, &wc) {
		obs.Class = "write_conflict"
		obs.Proto = map[string]any{
			"key_error": map[string]any{
				"conflict": map[string]any{
					"start_ts":           wc.StartTs,
					"conflict_ts":        wc.ConflictTs,
					"conflict_commit_ts": wc.ConflictCommitTs,
					"reason":             wc.Reason.String(),
				},
			},
		}
		return obs
	}

	// ── key exists ───────────────────────────────────────────────────────────
	// client-go error/error.go — ErrKeyExist wraps kvrpcpb.AlreadyExist, i.e. it
	// comes back from a real PREWRITE. Rust's DuplicateKeyInsertion is a CLIENT-SIDE
	// buffer check with no proto at all. Same class, and the `proto.present`
	// divergence is what exposes that they are not the same event.
	var ke *tikverr.ErrKeyExist
	if errors.As(err, &ke) {
		obs.Class = "key_exists"
		obs.Proto = map[string]any{
			"key_error": map[string]any{"already_exist": map[string]any{}},
		}
		return obs
	}

	var dl *tikverr.ErrDeadlock
	if errors.As(err, &dl) {
		obs.Class = "deadlock"
		obs.Proto = map[string]any{
			"key_error": map[string]any{"deadlock": map[string]any{}},
		}
		return obs
	}

	var af *tikverr.ErrAssertionFailed
	if errors.As(err, &af) {
		obs.Class = "assertion_failed"
		obs.Proto = map[string]any{
			"key_error": map[string]any{"assertion_failed": map[string]any{}},
		}
		return obs
	}

	var rt *tikverr.ErrRetryable
	if errors.As(err, &rt) {
		obs.Class = "retryable"
		return obs
	}

	// ── undetermined ─────────────────────────────────────────────────────────
	// The commit MAY have landed. Never roll back on this — doing so could tear a
	// committed batch. Both clients must surface it distinguishably.
	if tikverr.IsErrorUndetermined(err) {
		obs.Class = "undetermined"
		return obs
	}

	// ── txn not found — the string-matched wart (see txnNotFoundRe) ──────────
	if txnNotFoundRe.MatchString(err.Error()) {
		obs.Class = "txn_not_found"
		obs.Proto = map[string]any{
			"key_error": map[string]any{"txn_not_found": map[string]any{}},
		}
		return obs
	}

	// ── still-locked ─────────────────────────────────────────────────────────
	// A lock the client did NOT resolve. For client-go this is rare by construction:
	// its LockResolver resolves on read. When Rust surfaces this and Go does not,
	// that IS the finding.
	if strings.Contains(err.Error(), "key is locked") {
		obs.Class = "unresolved_lock"
		obs.Proto = map[string]any{
			"key_error": map[string]any{"locked": map[string]any{}},
		}
		return obs
	}

	// ── region / pd / rpc ────────────────────────────────────────────────────
	// A region error is a CLUSTER event, not client behaviour: the trace becomes
	// inadmissible and the runner retries, rather than reporting a divergence that
	// is really about the cluster.
	if strings.Contains(err.Error(), "region") && strings.Contains(err.Error(), "epoch") {
		obs.Class = "region_error"
		return obs
	}

	// ── THE CATCH-ALL: inadmissible, on purpose ──────────────────────────────
	// This is the whole defense. An error nobody mapped must NEVER be quietly
	// rendered as some plausible class — it becomes a run that cannot settle a
	// claim, and someone has to extend the taxonomy deliberately.
	obs.Class = "internal"
	obs.Detail = fmt.Sprintf("unmapped client-go error (%s): %s", obs.Native.Type, err.Error())
	return obs
}
