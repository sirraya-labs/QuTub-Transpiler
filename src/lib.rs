//! sirraya-qutub-transpiler
//!
//! A QASM 2.0 importer and native-gate compiler for circuits destined to
//! run on `sirraya_qutub::core::QuantumRegister`.
//!
//! Pipeline: [`qasm::parse`] (text -> [`ir::Circuit`]) ->
//! [`native::decompose`] ([`ir::Circuit`] -> [`native::NativeCircuit`]
//! over `{Rz, Ry, Rzz}`) -> [`optimize::optimize`] (peephole cleanup) ->
//! [`fidelity::estimate_circuit_fidelity`] (quick sanity-check number)
//! -> [`emit::run`] (actually execute it on `sirraya_qutub`).
//!
//! See each module's doc comment for the reasoning behind its piece of
//! the pipeline, and `tests/decompositions.rs` for the correctness
//! checks (run against the real dependency, not just asserted).

pub mod emit;
pub mod fidelity;
pub mod ir;
pub mod native;
pub mod optimize;
pub mod qasm;

pub use ir::{Circuit, Gate};
pub use native::{decompose, NativeCircuit, NativeGate};
pub use optimize::optimize;
pub use fidelity::{estimate_circuit_fidelity, PublishedCalibration};
