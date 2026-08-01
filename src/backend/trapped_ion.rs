//! Quantinuum-Helios-style trapped-ion [`BackendSpec`]: native gate set
//! `{Rz, Ry, Rzz}`, identical to [`crate::native::decompose`]'s own
//! canonical output -- see [`TrappedIonSpec::is_native_decompose_target`].

use crate::backend::{BackendCircuit, BackendGate};
use crate::backend::spec::{BackendSpec, RotAxis};
use crate::coupling::CouplingMap;
use crate::fidelity::PublishedCalibration;

pub(crate) struct TrappedIonSpec;

impl BackendSpec for TrappedIonSpec {
    fn id(&self) -> &'static str {
        "TrappedIon"
    }

    fn calibration(&self) -> PublishedCalibration {
        PublishedCalibration::quantinuum_helios_2026()
    }

    /// A trapped-ion chain's shared motional mode makes every qubit
    /// pair directly reachable -- there's nothing to route.
    fn coupling_map(&self, _num_qubits: usize) -> Option<CouplingMap> {
        None
    }

    fn rot_axis(&self) -> RotAxis {
        RotAxis::Ry
    }

    /// `Rzz` is already this backend's native two-qubit gate -- no
    /// re-expression needed, unlike `IbmQ`/`Rigetti`.
    fn push_two_qubit_zz(&self, bc: &mut BackendCircuit, a: usize, b: usize, theta: f64) {
        bc.push(BackendGate::Rzz(a, b, theta));
    }

    fn is_native_decompose_target(&self) -> bool {
        true
    }
}
