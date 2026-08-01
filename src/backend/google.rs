//! Google-superconducting-style [`BackendSpec`]: native gate set
//! `{Rz, Rx, Cz}`, modeling Willow (announced December 2024, Google's
//! current flagship processor) in its **CZ-tuned configuration**.
//!
//! # Why CZ, not the Sycamore gate or iSWAP
//! Google's tunable-coupler hardware doesn't have one fixed two-qubit
//! gate the way IBM's `Cx` or (mostly) Rigetti's `Cz` do -- the same
//! coupler can be tuned to different entangling interactions depending
//! on what a given experiment needs. Historically the headline gate was
//! the Sycamore gate itself (`FSimGate(pi/2, pi/6)`, used in the 2019
//! quantum-supremacy result) or its relative `sqrt(iSWAP)`, both of
//! which are excitation-preserving "hopping" gates -- algebraically a
//! genuinely different family from the diagonal `Cx`/`Cz`/`Rzz` gates
//! this crate's `push_two_qubit_zz` contract is built around (see
//! `backend/spec.rs`'s module doc on why a gate that doesn't fit that
//! shape doesn't belong in a `BackendSpec` impl at all). Willow's own
//! published spec sheet confirms the same tunable-coupler chip is
//! calibrated two different ways for two different jobs: a CZ-tuned
//! "Chip 1: Quantum Error Correction" configuration, and a separate
//! iSWAP-like-tuned "Chip 2: Random Circuit Sampling" configuration
//! (quantumai.google/static/site-assets/downloads/willow-spec-sheet.pdf,
//! Dec 9 2024). This implementation models **Chip 1's CZ-tuned mode**
//! specifically -- a real, officially published, first-class
//! configuration of the hardware, not a stand-in -- because CZ is a
//! diagonal gate this crate's existing, already-verified `Cz` identity
//! (see below) can lower to honestly. It does not model the iSWAP/
//! Sycamore-tuned configuration; that would need its own
//! excitation-preserving two-qubit gate representation, which is a
//! materially different (and unimplemented) extension -- the same
//! category of gap `backend.rs`'s module doc already documents for a
//! hypothetical photonic backend.
//!
//! # The gate identity itself: nothing new
//! Once CZ is the target, this is physically the same situation
//! `backend/rigetti.rs` already solved: a `Cz`-native, no-native-`Cx`
//! superconducting backend. Its `push_two_qubit_zz` reuses that exact
//! `H . Cz . Rx . Cz . H` identity via `backend`'s shared `push_h`
//! helper -- see `backend/rigetti.rs`'s own doc comment for the
//! derivation. This is deliberate: the identity doesn't know or care
//! which vendor's chip it's being lowered for, only that the backend is
//! `Cz`-native with an `Rx`-axis single-qubit gate, so there is nothing
//! to re-derive here, and a second independent implementation of the
//! same identity would only be a second place for it to silently drift
//! from the first.

use crate::backend::{push_h, BackendCircuit, BackendGate, EPS};
use crate::backend::spec::{BackendSpec, RotAxis};
use crate::coupling::CouplingMap;
use crate::fidelity::PublishedCalibration;

pub(crate) struct GoogleSpec;

impl BackendSpec for GoogleSpec {
    fn id(&self) -> &'static str {
        "Google"
    }

    fn calibration(&self) -> PublishedCalibration {
        PublishedCalibration::google_willow_2024()
    }

    /// Willow's own spec sheet reports "average connectivity 3.47
    /// (4-way typical)" across its 105 qubits -- a 2D grid with
    /// interior qubits coupled to their four neighbors, the same
    /// topology family `square_grid_for` already models for Rigetti.
    /// As with that backend, this is the topology *family*, not a
    /// reproduction of Willow's exact published qubit layout (see
    /// `coupling.rs`'s module doc on that distinction).
    fn coupling_map(&self, num_qubits: usize) -> Option<CouplingMap> {
        Some(CouplingMap::square_grid_for(num_qubits))
    }

    fn rot_axis(&self) -> RotAxis {
        RotAxis::Rx
    }

    /// Identical in form to `RigettiSpec::push_two_qubit_zz` -- see
    /// this module's doc comment for why that's the correct outcome,
    /// not a copy-paste that should have been factored out. (It *is*
    /// shared code already, via `push_h`; what's duplicated here is
    /// just the four-line assembly of `H`/`Cz`/`Rot`/`Cz`/`H` around
    /// it.)
    fn push_two_qubit_zz(&self, bc: &mut BackendCircuit, a: usize, b: usize, theta: f64) {
        if theta.abs() < EPS {
            return;
        }
        push_h(bc, RotAxis::Rx, b);
        bc.push(BackendGate::Cz(a, b));
        bc.push(BackendGate::Rot(b, theta)); // Rot == Rx on Google (Willow, CZ-tuned)
        bc.push(BackendGate::Cz(a, b));
        push_h(bc, RotAxis::Rx, b);
    }

    /// Identical in form and rationale to `RigettiSpec`'s override --
    /// see that module's doc comment on `has_native_cx`/`push_native_cx`
    /// for the identity (`Cx == H . Cz . H`) and why it's worth having.
    fn has_native_cx(&self) -> bool {
        true
    }

    fn push_native_cx(&self, bc: &mut BackendCircuit, control: usize, target: usize) {
        push_h(bc, RotAxis::Rx, target);
        bc.push(BackendGate::Cz(control, target));
        push_h(bc, RotAxis::Rx, target);
    }
}