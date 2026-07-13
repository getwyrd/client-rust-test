// THE DETERMINISTIC STATE FACTORY.
//
// client-go exports its 2PC internals from NON-test files — txnkv/transaction/
// test_probe.go carries no build tag — so an external module can drive a commit one
// phase at a time. That single fact is what makes this whole harness architecture pay
// for itself, and it is worth being precise about why.
//
// Finding 1's orphan is: a lock on a SECONDARY key whose PRIMARY was never written.
// Its primary has no lock and no write record, so a reader cannot learn the txn's fate
// from it and must escalate to `rollback_if_not_exist` once the TTL expires. client-go
// does. client-rust (#543) does not — the heal path is dead code.
//
// Manufacturing that with the public API alone is what gate.rs::d6 does, and it is
// brutal: force a PD region split so primary and secondary land in different regions,
// start an optimistic txn, lock the primary, put the secondary, race a THIRD txn to
// invalidate the primary so the orphaner loses at prewrite, drop without rollback —
// then retry the whole dance up to four times, re-establishing the region split each
// round because pd.toml's merge scheduler keeps undoing it.
//
// The trouble is not that it is ugly. It is that "I could not BUILD the orphan" and
// "the client could not RESOLVE the orphan" are the same red. d6 needs a
// PRECONDITION-FAILED panic and a failure-signature match precisely to tell those
// apart — the WRONG-FAILURE hazard in person.
//
// With CommitterProbe it is three calls and no races:
//
//	committer.SetPrimaryKey(primary)                        // a key we never prewrite
//	committer.PrewriteMutations(ctx, mutationsOf(secondary))
//	abandon
//
// No region split. No racing txn. No retry loop. And — the part that actually matters
// — the SETUP IS HELD CONSTANT ACROSS BOTH RUNS, so any divergence downstream is
// attributable to the READER alone. That is a strictly stronger claim than d6 can make.
package main

import (
	"context"
	"fmt"

	"github.com/tikv/client-go/v2/txnkv/transaction"
)

// A short TTL so the scenario's post-TTL read does not have to sleep for the client's
// default. The reader must be given a genuinely EXPIRED lock: that is the state in
// which resolution is unambiguously the client's job, and any lingering doubt about
// "maybe the txn is still alive" is removed.
const orphanLockTTLms = 1000

func (d *Driver) prewriteOnly(ctx context.Context, cmd Command) *Observation {
	txn, found := d.txns[cmd.Session]
	if !found {
		return driverError("prewrite_only: no such session: " + cmd.Session)
	}

	primary := cmd.Primary.Bytes()
	keys := make([][]byte, 0, len(cmd.Keys))
	for i := range cmd.Keys {
		keys = append(keys, cmd.Keys[i].Bytes())
	}
	if len(keys) == 0 {
		return driverError("prewrite_only: no keys to prewrite")
	}

	// The committer needs the txn's buffered mutations, so the scenario must have
	// `put` the keys first. NewCommitter reads the membuffer.
	probe := transaction.TxnProbe{KVTxn: txn}
	committer, err := probe.NewCommitter(1)
	if err != nil {
		return driverError(fmt.Sprintf("prewrite_only: NewCommitter: %v", err))
	}

	// The primary is named but NEVER prewritten — that is what makes the lock an
	// orphan rather than an ordinary in-flight 2PC.
	committer.SetPrimaryKey(primary)
	committer.SetLockTTL(orphanLockTTLms)

	mutations := committer.MutationsOfKeys(keys)
	if mutations.Len() != len(keys) {
		// The committer builds its mutations from the txn's membuffer, so the scenario
		// must have `put` every key it asks to prewrite. Catching this here turns a
		// baffling empty-message prewrite failure into a statement of what is wrong.
		return driverError(fmt.Sprintf(
			"prewrite_only: asked to prewrite %d key(s) but the committer holds %d matching mutation(s) — "+
				"the scenario must `put` each key before prewriting it",
			len(keys), mutations.Len()))
	}

	if err := committer.PrewriteMutations(ctx, mutations); err != nil {
		// A failed prewrite means we did not build the state the scenario needs.
		// driver_error, NOT a client observation: the run is inadmissible, and the
		// scenario must never proceed to "read the orphan" against a cluster where
		// no orphan exists. A vacuous green is the one outcome worse than a red.
		//
		// `%+v`, not `%v`: client-go wraps errors with pingcap/errors, and several
		// carry an EMPTY Error() string — `%v` renders them as nothing at all, which
		// is how this failure first presented. The stack is what makes it diagnosable.
		return driverError(fmt.Sprintf("prewrite_only: PrewriteMutations: [%T] %+v", err, err))
	}

	// Keep the committer alive so `abandon` can drop it without cleanup. If it were
	// garbage collected or cleaned up here, the residue under test would disappear.
	d.committers[cmd.Session] = &committer

	return ok().withStartTS(txn.StartTS())
}
