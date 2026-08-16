//! Executes a [`NativeCircuit`] against the real
//! `sirraya_qutub::core::QuantumRegister`, and emits it back out as
//! `sirraya_qutub`-dialect QASM text. This is the one module in the
//! crate that actually touches the dependency.
//!
//! `QuantumRegister::measure_single_qubit(&mut self, qubit: usize) ->
//! Result<u8, String>` is confirmed (source in hand, not just doc
//! comments) to perform a real Born-rule-sampled projective measurement
//! that collapses and renormalizes the state vector -- exactly the
//! primitive `Gate::Measure`'s P0.1 roadmap item was blocked on.
//! `apply_to`/`run` deliberately stay statevector-only (erroring on
//! `Measure`, same as before) since a caller of that entry point never
//! asked for classical output; `apply_to_with_measurement`/
//! `run_with_measurement` below are the real, confirmed answer for a
//! circuit that contains one.

use crate::native::{NativeCircuit, NativeGate};
use sirraya_qutub::core::QuantumRegister;

/// Runs the native circuit on a fresh `QuantumRegister` (starting in
/// |00...0>) and returns the resulting register.
pub fn run(circuit: &NativeCircuit) -> Result<QuantumRegister, String> {
    let mut reg = QuantumRegister::new(circuit.num_qubits)?;
    apply_to(circuit, &mut reg)?;
    Ok(reg)
}

/// Applies the native circuit's gates onto an existing register in
/// place, so callers can prepare a custom initial state first. Errors
/// on `Measure` -- this entry point returns only a `QuantumRegister`,
/// with nowhere to put a classical outcome; use
/// [`apply_to_with_measurement`] for a circuit that measures.
pub fn apply_to(circuit: &NativeCircuit, reg: &mut QuantumRegister) -> Result<(), String> {
    for gate in &circuit.gates {
        match *gate {
            NativeGate::Rz(q, angle) => reg.apply_rz(q, angle)?,
            NativeGate::Ry(q, angle) => reg.apply_ry(q, angle)?,
            NativeGate::Rzz(a, b, angle) => reg.apply_rzz(a, b, angle)?,
            NativeGate::Measure(..) => {
                return Err(
                    "NativeGate::Measure is not supported by emit::apply_to -- this entry \
                     point returns no classical outcomes; use apply_to_with_measurement (or \
                     run_with_measurement) instead"
                        .to_string(),
                );
            }
            NativeGate::If(..) => {
                return Err(
                    "NativeGate::If is not supported by emit::apply_to -- it must read a \
                     classical bit that no earlier gate in this entry point has anywhere to \
                     write; use apply_to_with_measurement (or run_with_measurement) instead"
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}

/// Runs the native circuit on a fresh `QuantumRegister` (starting in
/// |00...0>), returning both the final register *and* the classical
/// outcomes written by every `Measure` in the circuit (indexed by
/// classical bit, sized to `circuit.num_clbits`). This is the P0.1
/// roadmap item's `run_with_measurement`-style entry point.
pub fn run_with_measurement(
    circuit: &NativeCircuit,
) -> Result<(QuantumRegister, Vec<u8>), String> {
    let mut reg = QuantumRegister::new(circuit.num_qubits)?;
    let mut clbits = vec![0u8; circuit.num_clbits];
    apply_to_with_measurement(circuit, &mut reg, &mut clbits)?;
    Ok((reg, clbits))
}

/// As [`apply_to`], but `Measure(q, c)` is executed for real via
/// `QuantumRegister::measure_single_qubit`, collapsing `reg` and
/// writing the sampled outcome into `clbits[c]`. `clbits` must have at
/// least `circuit.num_clbits` entries -- `qasm::parse` already
/// range-checks every `Measure`'s `c` against the declared `creg` size,
/// so a caller building `clbits` as `vec![0u8; circuit.num_clbits]`
/// (as [`run_with_measurement`] does) can't index out of range here.
pub fn apply_to_with_measurement(
    circuit: &NativeCircuit,
    reg: &mut QuantumRegister,
    clbits: &mut [u8],
) -> Result<(), String> {
    for gate in &circuit.gates {
        apply_native_gate_with_measurement(gate, reg, clbits)?;
    }
    Ok(())
}

/// The actual per-gate execution [`apply_to_with_measurement`] and
/// [`NativeGate::If`]'s own conditional branch below both need --
/// factored out so `If`'s recursion into its `inner` doesn't need a
/// second copy of the `Rz`/`Ry`/`Rzz`/`Measure` dispatch.
fn apply_native_gate_with_measurement(
    gate: &NativeGate,
    reg: &mut QuantumRegister,
    clbits: &mut [u8],
) -> Result<(), String> {
    match *gate {
        NativeGate::Rz(q, angle) => reg.apply_rz(q, angle)?,
        NativeGate::Ry(q, angle) => reg.apply_ry(q, angle)?,
        NativeGate::Rzz(a, b, angle) => reg.apply_rzz(a, b, angle)?,
        NativeGate::Measure(q, c) => {
            let outcome = reg.measure_single_qubit(q)?;
            let num_clbits = clbits.len();
            let slot = clbits.get_mut(c).ok_or_else(|| {
                format!(
                    "Measure writes classical bit {} but only {} were provided",
                    c, num_clbits
                )
            })?;
            *slot = outcome;
        }
        NativeGate::If(clbit, value, ref inner) => {
            // This is the real thing `examples/quantum_teleportation.rs`
            // used to do by hand, directly against the `QuantumRegister`
            // after `emit::run` returned -- see that example's updated
            // doc comment for the before/after. `inner` is always a
            // bare Rz/Ry/Rzz (never Measure or another If -- see
            // `NativeGate::If`'s own doc comment), so this recursion
            // bottoms out in exactly one more call.
            let num_clbits = clbits.len();
            let outcome = *clbits.get(clbit).ok_or_else(|| {
                format!("If reads classical bit {} but only {} were provided", clbit, num_clbits)
            })?;
            if (outcome != 0) == value {
                apply_native_gate_with_measurement(inner, reg, clbits)?;
            }
        }
    }
    Ok(())
}

/// One native gate's statement text (no trailing `;`/newline), shared
/// by [`to_qasm`]/[`to_qasm3`] so the `rz`/`ry`/`rzz` spelling and the
/// `If`-prefix logic live in exactly one place. `measure_stmt` renders
/// `Measure`'s own dialect-specific spelling (arrow-style for 2.0,
/// assignment-style for 3.0 -- the one real difference between the two
/// emitters); everything else, including `If`, is identical between
/// them.
///
/// The `if (c[N]==0|1) <stmt>` syntax this emits for `NativeGate::If`
/// is this crate's own dialect extension, the same kind of deliberate,
/// documented departure from the standard `qasm.rs`'s module doc
/// already notes for `rzz`/`ryy` -- real OPENQASM 2.0's `if` conditions
/// on a whole `creg`'s *integer value*, not one indexed bit, and QASM
/// 3.0 has no single-statement (non-block) `if` at all. `qasm::parse`'s
/// `parse_if_condition` is the matching reader, so this still
/// round-trips exactly the way every other statement here does.
fn native_gate_stmt(gate: &NativeGate, measure_stmt: &dyn Fn(usize, usize) -> String) -> String {
    match *gate {
        NativeGate::Rz(q, angle) => format!("rz({}) q[{}]", angle, q),
        NativeGate::Ry(q, angle) => format!("ry({}) q[{}]", angle, q),
        NativeGate::Rzz(a, b, angle) => format!("rzz({}) q[{}], q[{}]", angle, a, b),
        NativeGate::Measure(q, c) => measure_stmt(q, c),
        NativeGate::If(clbit, value, ref inner) => format!(
            "if (c[{}]=={}) {}",
            clbit,
            value as u8,
            native_gate_stmt(inner, measure_stmt)
        ),
    }
}

/// `sirraya_qutub`-dialect OPENQASM 2.0 text for the native circuit,
/// using its own `rz`/`ry`/`rzz` mnemonics so it round-trips back
/// through [`crate::qasm::parse`].
pub fn to_qasm(circuit: &NativeCircuit, circuit_name: &str) -> String {
    let mut out = String::new();
    out.push_str("OPENQASM 2.0;\n");
    out.push_str("include \"qelib1.inc\";\n");
    out.push_str(&format!("qreg q[{}];\n", circuit.num_qubits));
    // NOTE: this used to write `creg c[{circuit.num_qubits}];` -- a
    // stand-in from before `Gate::Measure`/`num_clbits` existed, back
    // when nothing in the crate ever read a creg's declared size. Now
    // that `qasm::parse` validates a `measure` statement's classical
    // bit index against the declared creg size, emitting a size that
    // doesn't match `circuit.num_clbits` would make this output fail
    // to round-trip back through `qasm::parse` for any circuit with a
    // Measure whose clbit index is >= num_qubits.
    out.push_str(&format!("creg c[{}];\n", circuit.num_clbits));
    out.push_str(&format!(
        "// Circuit: {} (native gate set: rz, ry, rzz, measure, if)\n",
        circuit_name
    ));
    for gate in &circuit.gates {
        out.push_str(&native_gate_stmt(gate, &|q, c| format!("measure q[{}] -> c[{}]", q, c)));
        out.push_str(";\n");
    }
    out
}

/// `sirraya_qutub`-dialect OPENQASM **3.0** text for the native
/// circuit -- same `rz`/`ry`/`rzz` mnemonics and gate order as
/// [`to_qasm`], just spelled with QASM 3.0's register-declaration
/// (`qubit[n] q;` / `bit[n] c;`) and assignment-style measure
/// (`c[i] = measure q[j];`) syntax instead of 2.0's `qreg`/`creg`/
/// arrow-measure -- see [`crate::qasm`]'s module doc for the full
/// dialect comparison. Round-trips back through [`crate::qasm::parse`]
/// exactly as [`to_qasm`] does; the two differ only in which of
/// `parse`'s two recognized spellings they happen to emit; the
/// resulting `Circuit` a caller gets back is identical either way.
pub fn to_qasm3(circuit: &NativeCircuit, circuit_name: &str) -> String {
    let mut out = String::new();
    out.push_str("OPENQASM 3.0;\n");
    out.push_str("include \"stdgates.inc\";\n");
    out.push_str(&format!("qubit[{}] q;\n", circuit.num_qubits));
    // See to_qasm's note above on why this is circuit.num_clbits, not
    // circuit.num_qubits -- same reasoning applies here.
    out.push_str(&format!("bit[{}] c;\n", circuit.num_clbits));
    out.push_str(&format!(
        "// Circuit: {} (native gate set: rz, ry, rzz, measure, if)\n",
        circuit_name
    ));
    for gate in &circuit.gates {
        out.push_str(&native_gate_stmt(gate, &|q, c| format!("c[{}] = measure q[{}]", c, q)));
        out.push_str(";\n");
    }
    out
}

// ---------------------------------------------------------------------
// Backend-aware execution, additive to the NativeCircuit-only run/
// apply_to above. This re-expresses each BackendGate back in terms of
// qutub's own apply_* methods -- the exact same mapping already
// verified at fidelity 1.0 in backend.rs's own test module
// (apply_backend_circuit), just promoted from test-only code to a
// real public function so BackendCircuit can actually be executed,
// not just constructed.
// ---------------------------------------------------------------------

use crate::backend::{BackendCircuit, BackendGate, RotAxis};

/// Runs a lowered [`BackendCircuit`] on a fresh `QuantumRegister`
/// (starting in |00...0>) and returns the resulting register.
pub fn run_backend(circuit: &BackendCircuit) -> Result<QuantumRegister, String> {
    let mut reg = QuantumRegister::new(circuit.num_qubits)?;
    apply_backend_to(circuit, &mut reg)?;
    Ok(reg)
}

/// Applies a [`BackendCircuit`]'s gates onto an existing register in
/// place. `BackendGate::Rot` is interpreted as `Ry` for `TrappedIon`
/// and `Rx` for `IbmQ`/`Rigetti`, matching each backend's native
/// single-qubit axis (see `backend`'s module doc). Errors on `Measure`
/// for the same reason [`apply_to`] does; use
/// [`apply_backend_to_with_measurement`] for a circuit that measures.
pub fn apply_backend_to(circuit: &BackendCircuit, reg: &mut QuantumRegister) -> Result<(), String> {
    for gate in &circuit.gates {
        match *gate {
            BackendGate::Rz(q, angle) => reg.apply_rz(q, angle)?,
            BackendGate::Rot(q, angle) => match circuit.backend.rot_axis() {
                RotAxis::Ry => reg.apply_ry(q, angle)?,
                RotAxis::Rx => reg.apply_rx(q, angle)?,
            },
            BackendGate::Cx(a, b) => reg.apply_cnot(a, b)?,
            BackendGate::Cz(a, b) => reg.apply_controlled_z(a, b)?,
            BackendGate::Rzz(a, b, angle) => reg.apply_rzz(a, b, angle)?,
            BackendGate::Measure(..) => {
                return Err(
                    "BackendGate::Measure is not supported by emit::apply_backend_to -- this \
                     entry point returns no classical outcomes; use \
                     apply_backend_to_with_measurement (or run_backend_with_measurement) instead"
                        .to_string(),
                );
            }
            BackendGate::If(..) => {
                return Err(
                    "BackendGate::If is not supported by emit::apply_backend_to -- it must \
                     read a classical bit that no earlier gate in this entry point has \
                     anywhere to write; use apply_backend_to_with_measurement (or \
                     run_backend_with_measurement) instead"
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}

/// As [`run_backend`], but returns the classical outcomes written by
/// every `Measure` in the lowered circuit, indexed by classical bit.
pub fn run_backend_with_measurement(
    circuit: &BackendCircuit,
) -> Result<(QuantumRegister, Vec<u8>), String> {
    let mut reg = QuantumRegister::new(circuit.num_qubits)?;
    let mut clbits = vec![0u8; circuit.num_clbits];
    apply_backend_to_with_measurement(circuit, &mut reg, &mut clbits)?;
    Ok((reg, clbits))
}

/// As [`apply_backend_to`], but `Measure(q, c)` is executed for real,
/// same as [`apply_to_with_measurement`].
pub fn apply_backend_to_with_measurement(
    circuit: &BackendCircuit,
    reg: &mut QuantumRegister,
    clbits: &mut [u8],
) -> Result<(), String> {
    for gate in &circuit.gates {
        apply_backend_gate_with_measurement(circuit, gate, reg, clbits)?;
    }
    Ok(())
}

/// The actual per-gate execution [`apply_backend_to_with_measurement`]
/// and `BackendGate::If`'s own conditional branch below both need --
/// same factoring-out as [`apply_native_gate_with_measurement`], one
/// level up the pipeline. `circuit` is only needed for
/// `circuit.backend.rot_axis()` (which `BackendGate::Rot` alone can't
/// tell you -- see this module's doc comment on that variant).
fn apply_backend_gate_with_measurement(
    circuit: &BackendCircuit,
    gate: &BackendGate,
    reg: &mut QuantumRegister,
    clbits: &mut [u8],
) -> Result<(), String> {
    match *gate {
        BackendGate::Rz(q, angle) => reg.apply_rz(q, angle)?,
        BackendGate::Rot(q, angle) => match circuit.backend.rot_axis() {
            RotAxis::Ry => reg.apply_ry(q, angle)?,
            RotAxis::Rx => reg.apply_rx(q, angle)?,
        },
        BackendGate::Cx(a, b) => reg.apply_cnot(a, b)?,
        BackendGate::Cz(a, b) => reg.apply_controlled_z(a, b)?,
        BackendGate::Rzz(a, b, angle) => reg.apply_rzz(a, b, angle)?,
        BackendGate::Measure(q, c) => {
            let outcome = reg.measure_single_qubit(q)?;
            let num_clbits = clbits.len();
            let slot = clbits.get_mut(c).ok_or_else(|| {
                format!(
                    "Measure writes classical bit {} but only {} were provided",
                    c, num_clbits
                )
            })?;
            *slot = outcome;
        }
        BackendGate::If(clbit, value, ref inner) => {
            let num_clbits = clbits.len();
            let outcome = *clbits.get(clbit).ok_or_else(|| {
                format!("If reads classical bit {} but only {} were provided", clbit, num_clbits)
            })?;
            if (outcome != 0) == value {
                apply_backend_gate_with_measurement(circuit, inner, reg, clbits)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod qasm3_emit_tests {
    use super::*;

    fn sample_circuit() -> NativeCircuit {
        let mut nc = NativeCircuit::new(2);
        nc.num_clbits = 2;
        nc.push(NativeGate::Ry(0, std::f64::consts::FRAC_PI_4));
        nc.push(NativeGate::Rzz(0, 1, 1.2));
        nc.push(NativeGate::Measure(0, 0));
        nc.push(NativeGate::Measure(1, 1));
        nc
    }

    #[test]
    fn to_qasm3_round_trips_through_qasm_parse() {
        let nc = sample_circuit();
        let text = to_qasm3(&nc, "bell_like");
        let circuit = crate::qasm::parse(&text).expect("emitted QASM3 should re-parse");
        assert_eq!(circuit.num_qubits, 2);
        assert_eq!(circuit.num_clbits, 2);
        assert_eq!(
            circuit.gates,
            vec![
                crate::ir::Gate::Ry(0, std::f64::consts::FRAC_PI_4),
                crate::ir::Gate::Rzz(0, 1, 1.2),
                crate::ir::Gate::Measure(0, 0),
                crate::ir::Gate::Measure(1, 1),
            ]
        );
    }

    #[test]
    fn to_qasm_and_to_qasm3_parse_back_to_the_same_circuit() {
        let nc = sample_circuit();
        let c2 = crate::qasm::parse(&to_qasm(&nc, "x")).unwrap();
        let c3 = crate::qasm::parse(&to_qasm3(&nc, "x")).unwrap();
        assert_eq!(c2.gates, c3.gates);
        assert_eq!(c2.num_qubits, c3.num_qubits);
        assert_eq!(c2.num_clbits, c3.num_clbits);
    }

    #[test]
    fn to_qasm3_uses_qasm3_syntax_markers() {
        let nc = sample_circuit();
        let text = to_qasm3(&nc, "x");
        assert!(text.starts_with("OPENQASM 3.0;\n"));
        assert!(text.contains("include \"stdgates.inc\";"));
        assert!(text.contains("qubit[2] q;"));
        assert!(text.contains("bit[2] c;"));
        assert!(text.contains("c[0] = measure q[0];"));
        assert!(!text.contains("qreg"));
        assert!(!text.contains("->"));
    }

    fn conditioned_circuit() -> NativeCircuit {
        // The teleportation shape in miniature: measure, then a
        // conditioned correction.
        let mut nc = NativeCircuit::new(2);
        nc.num_clbits = 1;
        nc.push(NativeGate::Measure(0, 0));
        nc.push(NativeGate::If(0, true, Box::new(NativeGate::Rz(1, 0.5))));
        nc
    }

    #[test]
    fn to_qasm_emits_the_if_extension_syntax() {
        let text = to_qasm(&conditioned_circuit(), "cond");
        assert!(text.contains("if (c[0]==1) rz(0.5) q[1];"));
    }

    #[test]
    fn if_round_trips_through_qasm_parse() {
        let nc = conditioned_circuit();
        let circuit = crate::qasm::parse(&to_qasm(&nc, "cond")).unwrap();
        assert_eq!(
            circuit.gates,
            vec![
                crate::ir::Gate::Measure(0, 0),
                crate::ir::Gate::If(0, true, Box::new(crate::ir::Gate::Rz(1, 0.5))),
            ]
        );
    }

    #[test]
    fn if_round_trips_through_qasm3_parse_identically_to_qasm2() {
        let nc = conditioned_circuit();
        let c2 = crate::qasm::parse(&to_qasm(&nc, "cond")).unwrap();
        let c3 = crate::qasm::parse(&to_qasm3(&nc, "cond")).unwrap();
        assert_eq!(c2.gates, c3.gates);
    }
}
