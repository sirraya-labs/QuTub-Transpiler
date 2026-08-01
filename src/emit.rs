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
        }
    }
    Ok(())
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
        "// Circuit: {} (native gate set: rz, ry, rzz, measure)\n",
        circuit_name
    ));
    for gate in &circuit.gates {
        match *gate {
            NativeGate::Rz(q, angle) => out.push_str(&format!("rz({}) q[{}];\n", angle, q)),
            NativeGate::Ry(q, angle) => out.push_str(&format!("ry({}) q[{}];\n", angle, q)),
            NativeGate::Rzz(a, b, angle) => {
                out.push_str(&format!("rzz({}) q[{}], q[{}];\n", angle, a, b))
            }
            NativeGate::Measure(q, c) => {
                out.push_str(&format!("measure q[{}] -> c[{}];\n", q, c))
            }
        }
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
        }
    }
    Ok(())
}
