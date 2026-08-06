//! sirraya-qutub-transpiler
//!
//! A QASM 2.0 importer and multi-backend native-gate compiler for
//! circuits destined to run on `sirraya_qutub::core::QuantumRegister`.
//!
//! Pipeline: [`qasm::parse`] (text -> [`ir::Circuit`]) ->
//! [`ir_optimize::optimize`] (source-level cancellation/reordering) ->
//! [`backend::lower`] ([`ir::Circuit`] -> a target [`backend::Backend`]'s
//! native gate set, routing through [`route::route_best`] first against
//! a [`coupling::CouplingMap`] for any backend that isn't all-to-all --
//! `route_best` itself runs [`route::route`], [`route::route_lookahead`],
//! and [`route::route_sabre`] and keeps whichever used fewest SWAPs, so
//! `backend::lower` never has to choose between them itself; see
//! `route_best`'s own doc comment for why no single one of the three is
//! a strict improvement on the other two in every case) ->
//! [`optimize::optimize`] (native-level peephole cleanup) ->
//! [`fidelity::estimate_circuit_fidelity`] (quick sanity-check number)
//! -> [`emit::run`] / [`emit::run_backend`] (actually execute it on
//! `sirraya_qutub`). [`diagram::Diagram`] can render any of the three
//! circuit levels in this pipeline (source, native, or backend-lowered)
//! as an ASCII or SVG circuit diagram, independent of the rest of the
//! pipeline.
//!
//! [`pulse::schedule`] is a separate, optional downstream stage: it
//! lowers a [`BackendCircuit`] (the output of [`backend::lower`]) into
//! a hardware-channel [`pulse::Schedule`] against a per-backend
//! [`pulse::PulseCalibration`]. It sits below everything else in the
//! pipeline above and doesn't participate in it -- nothing upstream of
//! `backend::lower` needs to change, or even know it exists, for a
//! caller to opt into it.
//!
//! [`waveform_sim::integrate`] sits below `pulse` in turn: it doesn't
//! touch a [`pulse::Schedule`] at all, only a single
//! [`pulse::PulseInstruction::Play`] in isolation, numerically
//! simulating it against a two-level qubit to check whether a
//! `PulseCalibration`'s `rot` table is physically self-consistent --
//! the piece `pulse.rs` itself names as out of scope. Like `pulse`
//! relative to the rest of the pipeline, nothing above it needs to
//! change, or even know it exists.
//!
//! [`backend::Backend`] is an open extension point, not a fixed list:
//! it's a handle onto a [`backend::BackendSpec`] implementation,
//! and each of the three backends shipped today
//! (`backend/trapped_ion.rs`, `backend/ibmq.rs`, `backend/rigetti.rs`)
//! implements that trait in its own file. Adding a new backend means
//! implementing `BackendSpec` once, in a new file under `src/backend/`,
//! and registering one `Backend::` constant -- see `backend/spec.rs`'s
//! module doc for the design rationale and exactly what a new
//! implementation needs to provide (and its scope limits, illustrated
//! there by what a neutral-atom or photonic backend would additionally
//! require).
//!
//! See each module's doc comment for the reasoning behind its piece of
//! the pipeline, and `tests/decompositions.rs` for the correctness
//! checks against `native`/`optimize` (run against the real dependency,
//! not just asserted). `ir_optimize` and `backend` (and its `spec`
//! submodule) each carry their own `#[cfg(test)]` unit tests in-module.

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
pub mod pulse;
pub mod qasm;
pub mod route;
pub mod waveform_sim;

pub use backend::{lower, lower_with_coupling, Backend, BackendCircuit, BackendGate, BackendSpec, RotAxis};
pub use coupling::CouplingMap;
pub use diagram::{Diagram, DiagramInstr};
pub use ir::{Circuit, Gate};
pub use ir_optimize::optimize as optimize_ir;
pub use native::{decompose, NativeCircuit, NativeGate};
pub use optimize::optimize;
pub use pulse::{
    ibm_heron_r2_pulse_calibration, rigetti_ankaa3_pulse_calibration,
    trapped_ion_pulse_calibration, schedule, Channel, Envelope, PulseCalibration,
    PulseInstruction, Schedule, SingleQubitPulseCalibration, TwoQubitContinuousPulseCalibration,
    TwoQubitPulseCalibration,
};
pub use fidelity::{estimate_circuit_fidelity, PublishedCalibration};
pub use route::{route, route_lookahead, route_sabre, route_best, route_qft, restoration_swap_count};
pub use ibm_export::{to_ibm_qasm, lower_ibm_native, validate_cx_native_basis, IbmInstr};
pub use waveform_sim::{
    integrate, rotation_angle_rad, BlochVector, RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS,
};