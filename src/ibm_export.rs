//! Real IBM-hardware-native export for a lowered [`crate::backend::BackendCircuit`].
//!
//! `backend::BackendGate::Rot` models IBM's single-qubit gate as a
//! free-angle rotation about the X axis -- a useful simplification for
//! the routing/optimize/fidelity passes, but not something real IBM
//! hardware can actually pulse: IBM's physical single-qubit gate set is
//! just `Rz` (a free, zero-duration frame change -- "virtual Z") and
//! `SX` (a single fixed pi/2 pulse about X), plus `X` as its own
//! separately-calibrated gate. This module is the one place that
//! bridges the two: it expands every `Rot` into the real
//! `Rz`/`SX`/`Rz`/`SX`/`Rz` sequence IBM hardware actually executes,
//! and emits OPENQASM 2.0 text using IBM's own basis-gate names (`rz`,
//! `sx`, `x`, `cx`, `measure`) instead of `sirraya_qutub`'s own dialect
//! (see [`crate::emit::to_qasm`], which round-trips only through this
//! crate's own [`crate::qasm::parse`] and would not be accepted by
//! Qiskit or IBM's job-submission API as-is).
//!
//! # The `Rot` -> `Rz`/`SX` identity
//!
//! `SX` is `e^{i*pi/4} * Rx(pi/2)` (Qiskit's own matrix for it,
//! `0.5*[[1+i,1-i],[1-i,1+i]]`, is exactly that -- confirmed directly).
//! For any angle `theta`:
//!
//! ```text
//! Rx(theta) == Rz(pi/2) . SX . Rz(theta + pi) . SX . Rz(pi/2)
//! ```
//!
//! up to an unobservable global phase (never significant anywhere else
//! in this crate either -- see `native.rs`'s module doc). This was
//! derived symbolically (equating both sides' matrix entries and
//! solving for the two free `Rz` angles) and confirmed numerically
//! against a spread of `theta` values, including edge cases (`0`,
//! `pi`, negative, `> 2*pi`), *before* being written here -- not
//! assumed from memory or from a training-data recollection of a
//! similar-looking Qiskit identity. That numeric check is now also a
//! real regression test below
//! (`expand_rot_matches_rx_for_a_spread_of_angles`), reusing
//! `native.rs`'s already-tested `Rz`/`Rx` matrix builders rather than
//! re-deriving a second copy of the algebra.
//!
//! `expand_rot` special-cases `theta == 0` (identity, emits nothing)
//! and `theta == pi` mod `2*pi` (emits a bare `X`, IBM's own
//! separately-calibrated gate, instead of the generic 5-gate form)
//! since both are exact and strictly cheaper; every other angle uses
//! the general identity above.
//!
//! # What this deliberately does not do yet
//! This does not talk to IBM at all -- no credentials, no job
//! submission, no live device coupling map or basis-gate query. It
//! only turns an already-lowered, already-routed `BackendCircuit` (see
//! `backend::lower`, which already routes `IbmQ` circuits against
//! `coupling::CouplingMap::heavy_hex_for`) into text a real submission
//! path can consume. Submission itself is intentionally out of this
//! crate for now -- see the accompanying `submit_ibm.py` for that,
//! since there is no official Rust SDK for IBM Quantum Platform /
//! Qiskit Runtime.

use crate::backend::{Backend, BackendCircuit, BackendGate};

const EPS: f64 = 1e-9;
const TWO_PI: f64 = std::f64::consts::TAU;
const PI: f64 = std::f64::consts::PI;

/// One instruction in IBM's own real basis gate set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IbmInstr {
    /// Virtual Z -- free, zero-duration frame change.
    Rz(usize, f64),
    /// The single fixed pi/2-about-X pulse IBM hardware actually has.
    Sx(usize),
    /// IBM's own separately-calibrated X gate (not just two `Sx`'s --
    /// see this module's doc comment).
    X(usize),
    /// IBM's native two-qubit gate.
    Cx(usize, usize),
    Measure(usize, usize),
}

fn wrap_angle(theta: f64) -> f64 {
    let mut t = theta % TWO_PI;
    if t > PI {
        t -= TWO_PI;
    } else if t <= -PI {
        t += TWO_PI;
    }
    if t.abs() < EPS {
        t = 0.0;
    }
    t
}

/// Expands one `Rot(q, theta)` into the real gates IBM hardware runs,
/// per this module's doc comment.
fn expand_rot(q: usize, theta: f64, out: &mut Vec<IbmInstr>) {
    let t = wrap_angle(theta);
    if t == 0.0 {
        return;
    }
    if (t - PI).abs() < EPS || (t + PI).abs() < EPS {
        out.push(IbmInstr::X(q));
        return;
    }
    out.push(IbmInstr::Rz(q, PI / 2.0));
    out.push(IbmInstr::Sx(q));
    out.push(IbmInstr::Rz(q, wrap_angle(t + PI)));
    out.push(IbmInstr::Sx(q));
    out.push(IbmInstr::Rz(q, PI / 2.0));
}

/// Lowers a [`BackendCircuit`] already targeting [`Backend::IbmQ`]
/// (i.e. the output of `backend::lower(circuit, Backend::IbmQ)`) into
/// the real basis gates IBM hardware executes. Errors if `circuit`
/// wasn't lowered for `IbmQ` -- a `TrappedIon`/`Rigetti` circuit has no
/// `Rzz`/`Cz` equivalent here, since this module only ever bridges the
/// `IbmQ` model to IBM's real pulses.
pub fn lower_ibm_native(circuit: &BackendCircuit) -> Result<Vec<IbmInstr>, String> {
    if circuit.backend != Backend::IbmQ {
        return Err(format!(
            "lower_ibm_native only accepts a BackendCircuit lowered for Backend::IbmQ, got {:?}",
            circuit.backend
        ));
    }
    let mut out = Vec::with_capacity(circuit.gates.len());
    for gate in &circuit.gates {
        match *gate {
            BackendGate::Rz(q, a) => {
                let t = wrap_angle(a);
                if t != 0.0 {
                    out.push(IbmInstr::Rz(q, t));
                }
            }
            BackendGate::Rot(q, a) => expand_rot(q, a, &mut out),
            BackendGate::Cx(a, b) => out.push(IbmInstr::Cx(a, b)),
            BackendGate::Measure(q, c) => out.push(IbmInstr::Measure(q, c)),
            BackendGate::Cz(..) | BackendGate::Rzz(..) => {
                return Err(format!(
                    "lower_ibm_native: unexpected {:?} in an IbmQ-lowered circuit \
                     (Cz belongs to Rigetti, Rzz to TrappedIon -- backend::lower should \
                     never emit either for Backend::IbmQ)",
                    gate
                ));
            }
        }
    }
    Ok(merge_adjacent_rz(out))
}

/// Combines immediately-adjacent `Rz`s on the same qubit into one, and
/// drops any that net to zero -- purely cosmetic (virtual-Z is free
/// regardless of count), but keeps the exported QASM from carrying
/// redundant frame changes left over from back-to-back `expand_rot`
/// calls. Never merges across a `Measure` or a gate on a different
/// qubit -- same principle as `optimize.rs`'s peephole pass, and for
/// the same reason: nothing in between could have blocked commuting
/// them together, because they're already adjacent.
fn merge_adjacent_rz(instrs: Vec<IbmInstr>) -> Vec<IbmInstr> {
    let mut out: Vec<IbmInstr> = Vec::with_capacity(instrs.len());
    for instr in instrs {
        if let (Some(IbmInstr::Rz(q1, a1)), IbmInstr::Rz(q2, a2)) = (out.last().copied(), instr) {
            if q1 == q2 {
                let combined = wrap_angle(a1 + a2);
                out.pop();
                if combined != 0.0 {
                    out.push(IbmInstr::Rz(q1, combined));
                }
                continue;
            }
        }
        out.push(instr);
    }
    out
}

/// Checks a real backend's *own* published basis-gate list (e.g. from
/// `submit_ibm.py --dump-coupling-map`'s `basis_gates` field, itself
/// read off `BackendV2.target.operation_names` on the Qiskit side)
/// against the fixed assumption this module's `Rot`-expansion identity
/// and [`to_ibm_qasm`] are built on: `{rz, sx, x, cx, measure}`.
///
/// This module's own doc comment already states that basis as a
/// premise, not something derived per-device -- it was true of IBM's
/// CNOT-native generations, but several current-generation IBM devices
/// are **ECR**-native instead (`ecr` in place of `cx` as the native
/// two-qubit gate; a fixed-angle *echoed cross-resonance* gate, not
/// freely re-derivable from this module's `Rzz -> Cx . Rz . Cx`
/// identity). Calling [`to_ibm_qasm`] against an ECR-native backend
/// would silently emit `cx` instructions a submission pipeline then
/// has to itself re-transpile -- exactly the "no longer testing what
/// you think you're testing" failure mode this module's `Rot`
/// expansion identity was written to avoid for the single-qubit gates.
///
/// Returns `Ok(())` only if `basis_gates` contains `cx` (case-
/// insensitive) -- i.e. this module's assumed basis is actually a
/// subset of what the real device supports. Does **not** attempt to
/// generate an ECR-basis export itself: that needs its own derived
/// identity (`Rzz`/`Cx` re-expressed in terms of a fixed-angle `ECR`,
/// analogous to how `expand_rot` re-expresses `Rot` in terms of the
/// fixed `SX`), which is real follow-on work, not something this
/// check can paper over. Call this once, before [`to_ibm_qasm`], for
/// any circuit actually headed to real hardware rather than a
/// CX-native simulator.
pub fn validate_cx_native_basis(basis_gates: &[String]) -> Result<(), String> {
    let has_cx = basis_gates.iter().any(|g| g.eq_ignore_ascii_case("cx"));
    if has_cx {
        return Ok(());
    }
    let has_ecr = basis_gates.iter().any(|g| g.eq_ignore_ascii_case("ecr"));
    if has_ecr {
        return Err(
            "validate_cx_native_basis: this backend's basis gates include 'ecr', not 'cx' -- \
             it is ECR-native, not CX-native. ibm_export::to_ibm_qasm assumes a CX-native basis \
             ({rz, sx, x, cx, measure}, see this module's own doc comment) and will emit `cx` \
             instructions this backend cannot execute natively. An ECR-basis export needs its \
             own decomposition identity (Rzz/Cx re-expressed via the fixed-angle ECR gate), not \
             yet implemented here -- do not submit this export to an ECR-native backend."
                .to_string(),
        );
    }
    Err(format!(
        "validate_cx_native_basis: this backend's basis gates ({:?}) contain neither 'cx' nor \
         'ecr' -- ibm_export.rs's assumed CX-native basis ({{rz, sx, x, cx, measure}}) doesn't \
         match this device; check the backend's real basis gates before submitting",
        basis_gates
    ))
}

/// Real OPENQASM 2.0 text for an `IbmQ`-lowered [`BackendCircuit`],
/// using IBM's own basis gate names so it can be handed to Qiskit or
/// IBM's job-submission API directly.
pub fn to_ibm_qasm(circuit: &BackendCircuit, circuit_name: &str) -> Result<String, String> {
    let instrs = lower_ibm_native(circuit)?;
    let mut out = String::new();
    out.push_str("OPENQASM 2.0;\n");
    out.push_str("include \"qelib1.inc\";\n");
    out.push_str(&format!("qreg q[{}];\n", circuit.num_qubits));
    out.push_str(&format!("creg c[{}];\n", circuit.num_clbits));
    out.push_str(&format!(
        "// Circuit: {} (IBM native basis: rz, sx, x, cx, measure)\n",
        circuit_name
    ));
    for instr in &instrs {
        match *instr {
            IbmInstr::Rz(q, a) => out.push_str(&format!("rz({}) q[{}];\n", a, q)),
            IbmInstr::Sx(q) => out.push_str(&format!("sx q[{}];\n", q)),
            IbmInstr::X(q) => out.push_str(&format!("x q[{}];\n", q)),
            IbmInstr::Cx(a, b) => out.push_str(&format!("cx q[{}], q[{}];\n", a, b)),
            IbmInstr::Measure(q, c) => out.push_str(&format!("measure q[{}] -> c[{}];\n", q, c)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{approx_eq_up_to_global_phase, m_identity, m_rx, m_rz, matmul, Mat2};

    /// Rebuilds the exact matrix product `expand_rot` describes, for a
    /// given theta, and checks it against `m_rx(theta)` -- the same
    /// check this module's doc comment describes running externally
    /// (symbolic derivation + numeric sweep) before this identity was
    /// written down, now pinned as a real regression test instead of a
    /// one-off check. `Sx`/`X` are replayed as `m_rx(pi/2)`/`m_rx(pi)`
    /// respectively -- exact up to global phase, which is all this
    /// comparison ever checks (see `native.rs`'s module doc on why
    /// global phase is never observable anywhere in this crate).
    #[test]
    fn expand_rot_matches_rx_for_a_spread_of_angles() {
        let thetas = [
            0.0,
            0.001,
            0.3,
            1.0,
            std::f64::consts::FRAC_PI_2,
            std::f64::consts::PI,
            2.1,
            -0.7,
            -std::f64::consts::PI,
            5.5,
            -3.0,
            std::f64::consts::TAU,
        ];
        for &theta in &thetas {
            let mut instrs = Vec::new();
            expand_rot(0, theta, &mut instrs);

            let mut m: Mat2 = m_identity();
            for instr in &instrs {
                let g = match *instr {
                    IbmInstr::Rz(_, a) => m_rz(a),
                    IbmInstr::Sx(_) => m_rx(std::f64::consts::FRAC_PI_2),
                    IbmInstr::X(_) => m_rx(std::f64::consts::PI),
                    other => panic!("unexpected single-qubit instr {:?}", other),
                };
                // program order: first-applied is multiplied on the
                // right, matching native.rs's matmul convention.
                m = matmul(g, m);
            }
            assert!(
                approx_eq_up_to_global_phase(m, m_rx(theta)),
                "theta {}: expand_rot's gates don't match Rx(theta)",
                theta
            );
        }
    }

    #[test]
    fn expand_rot_is_empty_for_zero_angle() {
        let mut instrs = Vec::new();
        expand_rot(0, 0.0, &mut instrs);
        assert!(instrs.is_empty());
        let mut instrs2 = Vec::new();
        expand_rot(0, std::f64::consts::TAU, &mut instrs2);
        assert!(instrs2.is_empty());
    }

    #[test]
    fn expand_rot_uses_bare_x_at_pi() {
        let mut instrs = Vec::new();
        expand_rot(3, std::f64::consts::PI, &mut instrs);
        assert_eq!(instrs, vec![IbmInstr::X(3)]);
    }

    #[test]
    fn merge_adjacent_rz_combines_and_drops_zero() {
        let instrs = vec![IbmInstr::Rz(0, 0.3), IbmInstr::Rz(0, -0.3), IbmInstr::Sx(0)];
        let merged = merge_adjacent_rz(instrs);
        assert_eq!(merged, vec![IbmInstr::Sx(0)]);
    }

    #[test]
    fn merge_adjacent_rz_does_not_cross_a_different_qubit() {
        let instrs = vec![IbmInstr::Rz(0, 0.3), IbmInstr::Rz(1, 0.1), IbmInstr::Rz(0, 0.4)];
        let merged = merge_adjacent_rz(instrs.clone());
        assert_eq!(merged, instrs, "gates on q0 aren't adjacent, shouldn't merge");
    }

    #[test]
    fn validate_cx_native_basis_accepts_a_real_cx_native_basis() {
        let basis = vec!["rz".to_string(), "sx".to_string(), "x".to_string(), "cx".to_string()];
        assert!(validate_cx_native_basis(&basis).is_ok());
    }

    #[test]
    fn validate_cx_native_basis_is_case_insensitive() {
        let basis = vec!["RZ".to_string(), "SX".to_string(), "CX".to_string()];
        assert!(validate_cx_native_basis(&basis).is_ok());
    }

    #[test]
    fn validate_cx_native_basis_rejects_an_ecr_native_backend() {
        let basis = vec!["rz".to_string(), "sx".to_string(), "x".to_string(), "ecr".to_string()];
        let err = validate_cx_native_basis(&basis).unwrap_err();
        assert!(err.contains("ecr"), "error should name ECR as the mismatch: {}", err);
    }

    #[test]
    fn validate_cx_native_basis_rejects_an_unrecognized_basis() {
        let basis = vec!["rz".to_string(), "iswap".to_string()];
        assert!(validate_cx_native_basis(&basis).is_err());
    }

    #[test]
    fn rejects_non_ibmq_circuit() {
        let bc = BackendCircuit {
            backend: Backend::TrappedIon,
            num_qubits: 1,
            num_clbits: 0,
            gates: Vec::new(),
        };
        assert!(lower_ibm_native(&bc).is_err());
    }

    /// End-to-end sanity check on a real (tiny) circuit: a Bell pair,
    /// lowered through the actual `backend::lower` pipeline, must
    /// export without error and contain the expected gates -- not a
    /// decomposition-correctness check (that's the job of the
    /// `expand_rot`/native.rs tests), just confirming the plumbing
    /// between `backend::lower` and this module's export doesn't drop
    /// or duplicate anything.
    ///
    /// A source-level `Gate::Cx` on `IbmQ` costs exactly 1 native `Cx`,
    /// via `BackendSpec::push_native_cx`'s fast path (`Cx` is already
    /// IbmQ's native two-qubit gate -- see `backend/ibmq.rs`). Only a
    /// gate that genuinely needs the general `Rzz` canonical form (a
    /// non-trivial-angle `Rzz`, or any two-qubit gate reached via a
    /// path `push_native_cx` doesn't shortcut) pays the 2-native-`Cx`
    /// cost `push_two_qubit_zz`'s `Rzz(a,b,theta) ==
    /// Cx(a,b).Rz(b,theta).Cx(a,b)` identity describes -- see
    /// `backend.rs`'s own module doc and its
    /// `ibmq_rzz_costs_one_cx_pair`/`rigetti_and_ibmq_use_the_same_two_qubit_gate_count_for_rzz`
    /// tests for that case.
    #[test]
    fn bell_pair_exports_with_one_cx_and_two_measures() {
        use crate::ir::{Circuit, Gate};

        let mut circuit = Circuit::new(2);
        circuit.num_clbits = 2;
        circuit.push(Gate::H(0));
        circuit.push(Gate::Cx(0, 1));
        circuit.push(Gate::Measure(0, 0));
        circuit.push(Gate::Measure(1, 1));

        let bc = crate::backend::lower(&circuit, Backend::IbmQ);
        let instrs = lower_ibm_native(&bc).expect("IbmQ circuit should export cleanly");

        let cx_count = instrs.iter().filter(|i| matches!(i, IbmInstr::Cx(..))).count();
        let measure_count = instrs.iter().filter(|i| matches!(i, IbmInstr::Measure(..))).count();
        assert_eq!(
            cx_count, 1,
            "a source Cx is already IbmQ's native two-qubit gate; push_native_cx should \
             lower it directly to 1 native Cx, not round-trip through Rzz: {:?}",
            instrs
        );
        assert_eq!(measure_count, 2, "both qubits should be measured: {:?}", instrs);

        let qasm = to_ibm_qasm(&bc, "bell_pair").unwrap();
        assert!(qasm.contains("cx q["));
        assert!(qasm.contains("measure q["));
    }
}