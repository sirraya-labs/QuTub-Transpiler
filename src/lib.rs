//! sirraya-qutub-transpiler
//!
//! A QASM 2.0 importer and multi-backend native-gate compiler for
//! circuits destined to run on `sirraya_qutub::core::QuantumRegister`.
//!
//! Pipeline: [`qasm::parse`] (text -> [`ir::Circuit`]) ->
//! [`ir_optimize::optimize`] (source-level cancellation/reordering) ->
//! [`backend::lower`] ([`ir::Circuit`] -> a target [`backend::Backend`]'s
//! native gate set, routing through [`route::route`] first against a
//! [`coupling::CouplingMap`] for any backend that isn't all-to-all) ->
//! [`optimize::optimize`] (native-level peephole cleanup) ->
//! [`fidelity::estimate_circuit_fidelity`] (quick sanity-check number)
//! -> [`emit::run`] / [`emit::run_backend`] (actually execute it on
//! `sirraya_qutub`). [`diagram::Diagram`] can render any of the three
//! circuit levels in this pipeline (source, native, or backend-lowered)
//! as an ASCII or SVG circuit diagram, independent of the rest of the
//! pipeline.
//!
//! See each module's doc comment for the reasoning behind its piece of
//! the pipeline, and `tests/decompositions.rs` for the correctness
//! checks against `native`/`optimize` (run against the real dependency,
//! not just asserted). `ir_optimize` and `backend` each carry their own
//! `#[cfg(test)]` unit tests in-module.

pub mod backend;
pub mod coupling;
pub mod diagram;
pub mod emit;
pub mod fidelity;
pub mod ibm_export;
pub mod ir;
pub mod ir_optimize;
pub mod native;
pub mod optimize;
pub mod qasm;
pub mod route;

pub use backend::{lower, Backend, BackendCircuit, BackendGate};
pub use coupling::CouplingMap;
pub use diagram::{Diagram, DiagramInstr};
pub use ir::{Circuit, Gate};
pub use ir_optimize::optimize as optimize_ir;
pub use native::{decompose, NativeCircuit, NativeGate};
pub use optimize::optimize;
pub use fidelity::{estimate_circuit_fidelity, PublishedCalibration};
pub use route::route;
pub use ibm_export::{to_ibm_qasm, lower_ibm_native, IbmInstr};
