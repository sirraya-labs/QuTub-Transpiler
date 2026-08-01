//! IBM-superconducting-style [`BackendSpec`]: native gate set
//! `{Rz, Rx, Cx}` (virtual-Z framing + a native two-qubit `CNOT`).

use crate::backend::{BackendCircuit, BackendGate, EPS};
use crate::backend_spec::{BackendSpec, RotAxis};
use crate::coupling::CouplingMap;
use crate::fidelity::PublishedCalibration;

pub(crate) struct IbmQSpec;

impl BackendSpec for IbmQSpec {
    fn id(&self) -> &'static str {
        "IbmQ"
    }

    fn calibration(&self) -> PublishedCalibration {
        PublishedCalibration::ibm_heron_r2()
    }

    /// Routes against a real heavy-hex lattice -- IBM's actual
    /// published superconducting-device topology family (see
    /// `coupling.rs`).
    fn coupling_map(&self, num_qubits: usize) -> Option<CouplingMap> {
        Some(CouplingMap::heavy_hex_for(num_qubits))
    }

    fn rot_axis(&self) -> RotAxis {
        RotAxis::Rx
    }

    /// `Rzz(a, b, theta) == Cx(a, b) . Rz(b, theta) . Cx(a, b)` --
    /// exactly as cheap as `Rzz` is on `TrappedIon` (one native
    /// two-qubit gate).
    fn push_two_qubit_zz(&self, bc: &mut BackendCircuit, a: usize, b: usize, theta: f64) {
        if theta.abs() < EPS {
            return;
        }
        bc.push(BackendGate::Cx(a, b));
        bc.push(BackendGate::Rz(b, theta));
        bc.push(BackendGate::Cx(a, b));
    }
}
