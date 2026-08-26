//! Rigetti-superconducting-style [`BackendSpec`]: native gate set
//! `{Rz, Rx, Cz}` (`CZ`-native rather than `CNOT`-native).

use crate::backend::{push_h, BackendCircuit, BackendGate, EPS};
use crate::backend::spec::{BackendSpec, RotAxis};
use crate::coupling::CouplingMap;
use crate::fidelity::PublishedCalibration;

pub(crate) struct RigettiSpec;

impl BackendSpec for RigettiSpec {
    fn id(&self) -> &'static str {
        "Rigetti"
    }

    fn calibration(&self) -> PublishedCalibration {
        PublishedCalibration::rigetti_ankaa3()
    }

    /// Routes against a real square lattice -- Rigetti's actual
    /// published Ankaa-class topology family (see `coupling.rs`).
    fn coupling_map(&self, num_qubits: usize) -> Option<CouplingMap> {
        Some(CouplingMap::square_grid_for(num_qubits))
    }

    fn rot_axis(&self) -> RotAxis {
        RotAxis::Rx
    }

    /// Rigetti has no native `Cx`, so this doesn't naively substitute
    /// `Cx(a,b) == H(b).Cz(a,b).H(b)` into the `IbmQ` identity twice
    /// (which would cost 4 `H`'s). Instead:
    /// `H(b) . Rz(b, theta) . H(b) == Rx(b, theta)` (conjugating the
    /// Pauli `Z` generator by `H` gives `X`, so conjugating
    /// `Rz(theta) = exp(-i*theta*Z/2)` by `H` gives
    /// `Rx(theta) = exp(-i*theta*X/2)` at the operator-exponential
    /// level). Substituting this into the naive
    /// `H.Cz.H . Rz(theta) . H.Cz.H` expansion collapses the *middle*
    /// `H . Rz(theta) . H` into a single native `Rx` (this backend's
    /// `Rot`), leaving `H(b) . Cz(a,b) . Rx(b,theta) . Cz(a,b) . H(b)`
    /// -- 2 `H`'s instead of 4, same 2 `Cz`'s as the naive substitution.
    fn push_two_qubit_zz(&self, bc: &mut BackendCircuit, a: usize, b: usize, theta: f64) {
        if theta.abs() < EPS {
            return;
        }
        push_h(bc, RotAxis::Rx, b);
        bc.push(BackendGate::Cz(a, b));
        bc.push(BackendGate::Rot(b, theta)); // Rot == Rx on Rigetti
        bc.push(BackendGate::Cz(a, b));
        push_h(bc, RotAxis::Rx, b);
    }

    /// `Cx(control, target) == H(target) . Cz(control, target) . H(target)`
    /// -- the standard CNOT-from-CZ identity, and the same one already
    /// implicit in `native.rs`'s own `Cx` decomposition (`H` sandwiching
    /// a `CP(pi)`, which is `Cz` up to the global phase this crate never
    /// observes -- see `native.rs`'s `decompose_cp`). 1 native `Cz`,
    /// not the 2 the generic `push_two_qubit_zz` round-trip above would
    /// cost (see `BackendSpec::has_native_cx`'s doc comment).
    fn has_native_cx(&self) -> bool {
        true
    }

    fn push_native_cx(&self, bc: &mut BackendCircuit, control: usize, target: usize) {
        push_h(bc, RotAxis::Rx, target);
        bc.push(BackendGate::Cz(control, target));
        push_h(bc, RotAxis::Rx, target);
    }

    fn native_two_qubit_gate(&self) -> crate::backend::spec::NativeTwoQubitGate {
        crate::backend::spec::NativeTwoQubitGate::FixedCz
    }
}