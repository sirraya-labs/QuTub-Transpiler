//! Lowers a [`Circuit`] to a specific backend's native gate set, instead
//! of only the trapped-ion-style `{Rz, Ry, Rzz}` target in [`crate::native`].
//!
//! # Backends implemented
//! - [`Backend::TrappedIon`] -- `{Rz, Ry, Rzz}`. Delegates straight to
//!   [`crate::native::decompose`] (unchanged, already tested).
//! - [`Backend::IbmQ`] -- `{Rz, Rx, Cx}`, modeling IBM's superconducting
//!   basis (virtual-Z framing + a native two-qubit `CNOT`).
//! - [`Backend::Rigetti`] -- `{Rz, Rx, Cz}`, modeling Rigetti's
//!   superconducting basis (`CZ`-native rather than `CNOT`-native).
//!
//! Two new circuit identities do the actual work here, on top of the
//! ones already in `native.rs`:
//! 1. `Ry(theta) == Rx(-pi/2) . Rz(theta) . Rx(pi/2)` -- reused
//!    directly from the `RYY` decomposition in `native.rs` (same
//!    Y = Rx(-pi/2).Z.Rx(pi/2) fact, exponentiated), so IBMQ/Rigetti's
//!    single-qubit gates reuse the *same* ZYZ synthesis as the
//!    trapped-ion target and just re-express the resulting `Ry` calls.
//! 2. `Rzz(a, b, theta) == Cx(a, b) . Rz(b, theta) . Cx(a, b)` -- new,
//!    and the reason `Cx` is exactly as cheap on `IbmQ` as `Rzz` is on
//!    `TrappedIon` (one native two-qubit gate), while every *other*
//!    two-qubit gate that isn't already `Cx` costs more.
//!
//! `Rigetti` (`Cz`-native, no native `Cx`) is built by lowering the same
//! `Cx(a, b, theta)`-shaped intermediate through
//! `Cx(a, b) == H(b) . Cz(a, b) . H(b)` (already used in `native.rs` for
//! the trapped-ion target) -- so a `Rigetti` `Rzz` costs 2 `Cz`s, not 1.
//!
//! # What's not here: Pasqal (neutral atoms)
//! Neutral-atom platforms (Pasqal, and analog/digital Rydberg-blockade
//! devices generally) aren't a fourth entry in this same enum on
//! purpose. Their native "two-qubit gate" is a blockade interaction
//! between whichever atoms are currently within blockade radius of each
//! other in a *movable, laser-tweezer-defined* 2D/3D layout -- so
//! "compiling to Pasqal's native gates" is inseparable from *placing*
//! the atoms and routing which pairs are ever simultaneously in
//! blockade range, which is a materially different problem from
//! "express this unitary in terms of a fixed two-qubit gate" (the
//! problem this module and `native.rs` solve). Pasqal does also expose
//! a "digital" mode with a fixed local `CZ`-like gate (making it
//! superficially similar to `Rigetti` here), but modeling it correctly
//! still needs blockade-radius/layout constraints this crate doesn't
//! have -- shipping a `Backend::Pasqal` that reused the `Rigetti` path
//! under a different name would be presenting an untested, physically
//! incomplete backend as equivalent to the two above, which were tested
//! the same way `native.rs` was. Left as a follow-on.

use crate::ir::{Circuit, Gate};
use std::f64::consts::FRAC_PI_2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    TrappedIon,
    IbmQ,
    Rigetti,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackendGate {
    Rz(usize, f64),
    /// The backend's native continuously-variable single-qubit
    /// rotation: `Ry` for `TrappedIon`, `Rx` for `IbmQ`/`Rigetti`. (For
    /// `IbmQ`/`Rigetti` this is a modeling simplification of a fixed
    /// `SX`/`SX-dagger` pulse as a continuously-parameterized rotation
    /// about the same axis -- see this module's doc comment.)
    Rot(usize, f64),
    /// `IbmQ`'s native two-qubit gate.
    Cx(usize, usize),
    /// `Rigetti`'s native two-qubit gate.
    Cz(usize, usize),
    /// `TrappedIon`'s native two-qubit gate.
    Rzz(usize, usize, f64),
}

#[derive(Debug, Clone)]
pub struct BackendCircuit {
    pub backend: Backend,
    pub num_qubits: usize,
    pub gates: Vec<BackendGate>,
}

impl BackendCircuit {
    fn new(backend: Backend, num_qubits: usize) -> Self {
        Self {
            backend,
            num_qubits,
            gates: Vec::new(),
        }
    }
    fn push(&mut self, g: BackendGate) {
        self.gates.push(g);
    }

    /// (single_qubit_gate_count, two_qubit_gate_count) -- the two
    /// numbers a per-backend fidelity budget needs.
    pub fn gate_counts(&self) -> (usize, usize) {
        let mut single = 0;
        let mut two = 0;
        for g in &self.gates {
            match g {
                BackendGate::Rz(..) | BackendGate::Rot(..) => single += 1,
                BackendGate::Cx(..) | BackendGate::Cz(..) | BackendGate::Rzz(..) => two += 1,
            }
        }
        (single, two)
    }
}

const EPS: f64 = 1e-9;

/// Lowers a source-level circuit straight to `backend`'s native gate set.
pub fn lower(circuit: &Circuit, backend: Backend) -> BackendCircuit {
    match backend {
        Backend::TrappedIon => {
            // Already-tested path: reuse native.rs verbatim.
            let native = crate::native::decompose(circuit);
            let mut bc = BackendCircuit::new(backend, circuit.num_qubits);
            for g in &native.gates {
                bc.push(match *g {
                    crate::native::NativeGate::Rz(q, a) => BackendGate::Rz(q, a),
                    crate::native::NativeGate::Ry(q, a) => BackendGate::Rot(q, a),
                    crate::native::NativeGate::Rzz(a, b, t) => BackendGate::Rzz(a, b, t),
                });
            }
            bc
        }
        Backend::IbmQ | Backend::Rigetti => {
            // Reuse the same Rz/Ry/Rzz canonical form (native.rs), then
            // re-express each gate in terms of this backend's native
            // Rx/Cx (IbmQ) or Rx/Cz (Rigetti).
            let native = crate::native::decompose(circuit);
            let mut bc = BackendCircuit::new(backend, circuit.num_qubits);
            for g in &native.gates {
                match *g {
                    crate::native::NativeGate::Rz(q, a) => bc.push(BackendGate::Rz(q, a)),
                    crate::native::NativeGate::Ry(q, a) => push_ry_via_rx(&mut bc, q, a),
                    crate::native::NativeGate::Rzz(a, b, t) => push_rzz(&mut bc, backend, a, b, t),
                }
            }
            bc
        }
    }
}

/// `Ry(theta) == Rx(-pi/2) . Rz(theta) . Rx(pi/2)` (apply `Rx(pi/2)`
/// first, then `Rz(theta)`, then `Rx(-pi/2)` last). See this module's
/// doc comment, identity 1.
fn push_ry_via_rx(bc: &mut BackendCircuit, q: usize, theta: f64) {
    bc.push(BackendGate::Rot(q, FRAC_PI_2));
    bc.push(BackendGate::Rz(q, theta));
    bc.push(BackendGate::Rot(q, -FRAC_PI_2));
}

/// `Rzz(a, b, theta) == Cx(a, b) . Rz(b, theta) . Cx(a, b)`, further
/// lowered to `Cz`-only for `Rigetti` via `Cx == H(b) . Cz(a, b) . H(b)`
/// (with `H` itself expressed as `Rx`/`Rz`, same as every other
/// single-qubit gate here). See this module's doc comment, identity 2.
fn push_rzz(bc: &mut BackendCircuit, backend: Backend, a: usize, b: usize, theta: f64) {
    if theta.abs() < EPS {
        return;
    }
    match backend {
        Backend::IbmQ => {
            bc.push(BackendGate::Cx(a, b));
            bc.push(BackendGate::Rz(b, theta));
            bc.push(BackendGate::Cx(a, b));
        }
        Backend::Rigetti => {
            push_cx_via_cz(bc, a, b);
            bc.push(BackendGate::Rz(b, theta));
            push_cx_via_cz(bc, a, b);
        }
        Backend::TrappedIon => unreachable!("push_rzz only called for IbmQ/Rigetti"),
    }
}

/// `Cx(a, b) == H(b) . Cz(a, b) . H(b)`, with `H` itself lowered by
/// re-running it through the *same* `native::decompose` +
/// `push_ry_via_rx` path every other single-qubit gate takes here
/// (rather than hand-deriving `H`'s specific `Rz`/`Ry` angles a second
/// time and risking a fresh sign error the way the first version of
/// `native.rs`'s ZYZ synthesis did).
fn push_cx_via_cz(bc: &mut BackendCircuit, a: usize, b: usize) {
    push_h(bc, b);
    bc.push(BackendGate::Cz(a, b));
    push_h(bc, b);
}

fn push_h(bc: &mut BackendCircuit, q: usize) {
    let mut h_circuit = Circuit::new(q + 1);
    h_circuit.push(Gate::H(q));
    let canonical = crate::native::decompose(&h_circuit);
    for g in &canonical.gates {
        match *g {
            crate::native::NativeGate::Rz(qq, a) => bc.push(BackendGate::Rz(qq, a)),
            crate::native::NativeGate::Ry(qq, a) => push_ry_via_rx(bc, qq, a),
            crate::native::NativeGate::Rzz(..) => unreachable!("H never decomposes to Rzz"),
        }
    }
}

impl Backend {
    /// The [`crate::fidelity::PublishedCalibration`] matching this
    /// backend's modeled hardware, so a `BackendCircuit`'s fidelity can
    /// be estimated with the right published numbers for the gate set
    /// it was actually lowered to -- using `TrappedIon`'s
    /// `quantinuum_helios_2026()` figures against an `IbmQ` gate count
    /// would silently mix hardware that was never benchmarked together.
    pub fn calibration(self) -> crate::fidelity::PublishedCalibration {
        match self {
            Backend::TrappedIon => crate::fidelity::PublishedCalibration::quantinuum_helios_2026(),
            Backend::IbmQ => crate::fidelity::PublishedCalibration::ibm_heron_r2(),
            Backend::Rigetti => crate::fidelity::PublishedCalibration::rigetti_ankaa3(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit;
    use crate::optimize::optimize as native_optimize;
    use rand::Rng;
    use sirraya_qutub::core::QuantumRegister;

    const TOL: f64 = 1e-9;

    fn randomized_register(num_qubits: usize) -> QuantumRegister {
        let mut reg = QuantumRegister::new(num_qubits).unwrap();
        let mut rng = rand::thread_rng();
        for q in 0..num_qubits {
            reg.apply_rz(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
            reg.apply_ry(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
            reg.apply_rz(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
        }
        reg
    }

    /// Applies a `BackendCircuit` directly to a `QuantumRegister`,
    /// re-expressing each backend-native gate back in terms of qutub's
    /// own `apply_*` methods (`Cx` -> `apply_cnot`, `Cz` ->
    /// `apply_controlled_z`, `Rot` -> `apply_ry`/`apply_rx` depending on
    /// backend).
    fn apply_backend_circuit(bc: &BackendCircuit, reg: &mut QuantumRegister) {
        for g in &bc.gates {
            match *g {
                BackendGate::Rz(q, a) => reg.apply_rz(q, a).unwrap(),
                BackendGate::Rot(q, a) => match bc.backend {
                    Backend::TrappedIon => reg.apply_ry(q, a).unwrap(),
                    Backend::IbmQ | Backend::Rigetti => reg.apply_rx(q, a).unwrap(),
                },
                BackendGate::Cx(a, b) => reg.apply_cnot(a, b).unwrap(),
                BackendGate::Cz(a, b) => reg.apply_controlled_z(a, b).unwrap(),
                BackendGate::Rzz(a, b, t) => reg.apply_rzz(a, b, t).unwrap(),
            }
        }
    }

    fn check_backend_matches(circuit: &Circuit, backend: Backend) {
        let mut direct = randomized_register(circuit.num_qubits);
        let mut lowered_reg = direct.clone();

        // Ground truth: same circuit, but through the already-tested
        // native {Rz,Ry,Rzz} + optimize path via emit::apply_to.
        let native = native_optimize(&crate::native::decompose(circuit));
        emit::apply_to(&native, &mut direct).unwrap();

        let bc = lower(circuit, backend);
        apply_backend_circuit(&bc, &mut lowered_reg);

        let fidelity = direct.fidelity(&lowered_reg).unwrap();
        assert!(
            (fidelity - 1.0).abs() < TOL,
            "backend {:?}: fidelity {} (gates: {:?})",
            backend,
            fidelity,
            bc.gates
        );
    }

    fn sample_circuit() -> Circuit {
        let mut c = Circuit::new(3);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 1))
            .push(Gate::Rz(2, 0.37))
            .push(Gate::Cx(1, 2))
            .push(Gate::T(0))
            .push(Gate::Ryy(0, 2, 0.91))
            .push(Gate::Swap(1, 2))
            .push(Gate::Cp(0, 1, 1.2));
        c
    }

    #[test]
    fn trapped_ion_matches_native_path() {
        check_backend_matches(&sample_circuit(), Backend::TrappedIon);
    }

    #[test]
    fn ibmq_matches_native_path() {
        check_backend_matches(&sample_circuit(), Backend::IbmQ);
    }

    #[test]
    fn rigetti_matches_native_path() {
        check_backend_matches(&sample_circuit(), Backend::Rigetti);
    }

    #[test]
    fn ibmq_rzz_costs_one_cx_pair() {
        let mut c = Circuit::new(2);
        c.push(Gate::Rzz(0, 1, 0.5));
        let bc = lower(&c, Backend::IbmQ);
        let cx_count = bc.gates.iter().filter(|g| matches!(g, BackendGate::Cx(..))).count();
        assert_eq!(cx_count, 2, "Rzz should lower to exactly 2 Cx on IbmQ");
    }

    #[test]
    fn rigetti_and_ibmq_use_the_same_two_qubit_gate_count_for_rzz() {
        // Both go through the same Cx(a,b).Rz(b,theta).Cx(a,b)
        // intermediate (2 two-qubit gates); Rigetti just re-expresses
        // each of those 2 Cx's as 1 Cz (via Cx == H.Cz.H), so the
        // two-qubit *count* comes out equal -- 2 Cx vs 2 Cz, not more.
        let mut c = Circuit::new(2);
        c.push(Gate::Rzz(0, 1, 0.5));
        let (_, ibmq_two) = lower(&c, Backend::IbmQ).gate_counts();
        let (_, rigetti_two) = lower(&c, Backend::Rigetti).gate_counts();
        assert_eq!(
            ibmq_two, rigetti_two,
            "expected equal 2Q gate counts (2 Cx vs 2 Cz) for the same Rzz: {} vs {}",
            ibmq_two, rigetti_two
        );
    }

    #[test]
    fn rigetti_rzz_costs_more_single_qubit_gates_than_ibmq() {
        // This is where Rigetti actually pays more: each of its 2 Cx's
        // (via Cx == H.Cz.H) needs 2 bracketing H gates that IbmQ's
        // native Cx doesn't need at all.
        let mut c = Circuit::new(2);
        c.push(Gate::Rzz(0, 1, 0.5));
        let (ibmq_single, _) = lower(&c, Backend::IbmQ).gate_counts();
        let (rigetti_single, _) = lower(&c, Backend::Rigetti).gate_counts();
        assert!(
            rigetti_single > ibmq_single,
            "Rigetti's Cx-via-Cz should need more single-qubit gates than IbmQ's native Cx: {} vs {}",
            rigetti_single,
            ibmq_single
        );
    }
    #[test]
    fn each_backend_calibration_gives_a_fidelity_estimate() {
        use crate::fidelity::estimate_backend_circuit_fidelity;
        for backend in [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti] {
            let bc = lower(&sample_circuit(), backend);
            let cal = backend.calibration();
            let fidelity = estimate_backend_circuit_fidelity(&bc, &cal);
            assert!(
                fidelity > 0.0 && fidelity <= 1.0,
                "backend {:?}: fidelity estimate {} out of range",
                backend,
                fidelity
            );
        }
    }
}