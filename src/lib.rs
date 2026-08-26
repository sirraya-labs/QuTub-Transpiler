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
//! [`sequencer::compile`] sits below `pulse` a different way: rather
//! than going deeper into single-pulse physics the way `waveform_sim`
//! does, it goes sideways into real-time *control flow* -- turning an
//! already-lowered [`BackendCircuit`] (containing a
//! [`BackendGate::If`]) into a branching [`sequencer::Program`] with
//! real registers and real jumps, the layer `pulse::Schedule`'s own
//! flat, unconditional instruction list deliberately doesn't attempt
//! (see `pulse.rs`'s own `BackendGate::If` handling, and
//! `sequencer.rs`'s module doc for the full rationale). This is the
//! layer a real control-electronics backend (Quantum Machines, Zurich
//! Instruments, Keysight, or a lab's own custom stack) would target;
//! [`sequencer::HardwareTarget`] is the open extension point for one,
//! mirroring [`backend::BackendSpec`]'s one-trait-per-vendor pattern --
//! no implementation ships in this crate yet, since guessing at a
//! specific vendor's real instruction set isn't something this crate
//! does (see that module's doc comment).
//!
//! [`readout`] is a different kind of noise from anything else above:
//! not a gate-error or pulse-fidelity budget, but the classical
//! confusion between a qubit's true, exactly-collapsed measurement
//! outcome and what a real (imperfect) readout chain actually reports.
//! [`sequencer::execute_with_readout_noise`] is where it plugs in --
//! applied to exactly the classical bit `Gate::If`/`SeqInstr::JumpIfEqual`
//! branch on, so a corrupted readout can make a conditioned correction
//! fire (or not) based on the wrong bit, even though the underlying
//! quantum collapse stayed exact. See `readout.rs`'s own doc comment
//! for why this is modeled as an independent, purely classical layer
//! rather than folded into any other noise source in this crate.
//!
//! [`noise`] is the gate-error counterpart: real, per-gate depolarizing
//! noise sampled from [`fidelity::PublishedCalibration`]'s existing
//! cited numbers (the same ones `fidelity::estimate_circuit_fidelity`
//! already uses for its own aggregate survival-probability estimate,
//! now made sampleable rather than only summarized). Every example in
//! this crate that needed a genuinely noisy run used to hand-roll its
//! own ad hoc version of this at the example level -- see `noise.rs`'s
//! own doc comment for why that stopped being the right call once
//! `sequencer::execute_with_noise` needed the same thing in the
//! library itself, combined with [`readout`] noise in one place.
//!
//! [`resource_estimate`] is a separate, additive concern from
//! everything above: it never touches `sirraya_qutub` at all (it's
//! pure counting over an [`ir::Circuit`], via [`native::decompose`] +
//! [`optimize::optimize`]), and estimates a *fault-tolerant* resource
//! budget (T-count, T-depth) rather than [`fidelity`]'s NISQ-era
//! depolarizing-error budget. It exists because "how many T gates does
//! this circuit need" is the number a fault-tolerant target actually
//! gets designed and rejected against, the way `fidelity`'s estimate
//! already serves that role for NISQ backends -- see its own module
//! doc for what it does and deliberately does not estimate (physical
//! qubit count and code distance are real, separate, hardware-specific
//! follow-on work).
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
pub mod noise;
pub mod optimize;
pub mod pulse;
pub mod qasm;
pub mod readout;
pub mod resource_estimate;
pub mod route;
pub mod sequencer;
pub mod waveform_sim;

pub use backend::{lower, lower_with_coupling, lower_no_restore, lower_with_coupling_no_restore, Backend, BackendCircuit, BackendGate, BackendSpec, RotAxis};
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
pub use sequencer::{compile as compile_sequencer, execute as execute_sequencer, HardwareTarget, Program, SeqInstr};
pub use readout::{corrupt_readout, ReadoutCalibration};
pub use noise::{apply_pauli_error, sample_depolarizing_error, PauliError};
pub use fidelity::{estimate_circuit_fidelity, PublishedCalibration};
pub use route::{route, route_lookahead, route_sabre, route_best, route_best_no_restore, route_qft, restoration_swap_count};
pub use ibm_export::{to_ibm_qasm, lower_ibm_native, validate_cx_native_basis, IbmInstr};
pub use waveform_sim::{
    integrate, rotation_angle_rad, BlochVector, RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS,
};
pub use resource_estimate::{
    estimate_circuit_resources, estimate_circuit_resources_with_epsilon, ResourceBudget,
    RotationSynthesis, DEFAULT_ROTATION_EPSILON,
};