//! The parity protocol: the schema both drivers speak, and the projection that makes
//! two clients written in different languages comparable.
//!
//! **This crate deliberately depends on NEITHER client.** It is the contract, not a
//! party to it. `parity-runner` — the thing that decides a verdict — depends on this
//! and therefore cannot reach for `tikv-client`: the comparator must not be able to
//! touch the subject under test. Each driver owns its OWN half of the mapping table
//! (`rust-driver/src/mapping.rs`, `go/driver/observation.go`), because a mapping from
//! a client's errors necessarily depends on that client.
//!
//! The design in one line: **canonicalize the question, never the evidence.**
//! Traces are captured raw; [`project`] decides what a given claim looks at. A
//! projection can be widened later; evidence discarded at capture is gone.

pub mod class;
pub mod command;
pub mod observation;
pub mod project;
pub mod trace;

pub use class::Class;
pub use command::Command;
pub use command::Hello;
pub use command::KeyArg;
pub use command::TxnMode;
pub use command::PROTOCOL_VERSION;
pub use observation::Bytes;
pub use observation::ChecksumObs;
pub use observation::LockObs;
pub use observation::NativeObs;
pub use observation::Observation;
pub use project::diff;
pub use project::project;
pub use project::Divergence;
pub use project::Spec;
pub use trace::RoleBinding;
pub use trace::Step;
pub use trace::Trace;
pub use trace::TRACE_SCHEMA;
