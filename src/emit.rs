//! Executes a [`NativeCircuit`] against the real
//! `sirraya_qutub::core::QuantumRegister`, and emits it back out as
//! `sirraya_qutub`-dialect QASM text. This is the one module in the
//! crate that actually touches the dependency.

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
/// place, so callers can prepare a custom initial state first.
pub fn apply_to(circuit: &NativeCircuit, reg: &mut QuantumRegister) -> Result<(), String> {
    for gate in &circuit.gates {
        match *gate {
            NativeGate::Rz(q, angle) => reg.apply_rz(q, angle)?,
            NativeGate::Ry(q, angle) => reg.apply_ry(q, angle)?,
            NativeGate::Rzz(a, b, angle) => reg.apply_rzz(a, b, angle)?,
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
    out.push_str(&format!("creg c[{}];\n", circuit.num_qubits));
    out.push_str(&format!("// Circuit: {} (native gate set: rz, ry, rzz)\n", circuit_name));
    for gate in &circuit.gates {
        match *gate {
            NativeGate::Rz(q, angle) => out.push_str(&format!("rz({}) q[{}];\n", angle, q)),
            NativeGate::Ry(q, angle) => out.push_str(&format!("ry({}) q[{}];\n", angle, q)),
            NativeGate::Rzz(a, b, angle) => {
                out.push_str(&format!("rzz({}) q[{}], q[{}];\n", angle, a, b))
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

use crate::backend::{Backend, BackendCircuit, BackendGate};

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
/// single-qubit axis (see `backend`'s module doc).
pub fn apply_backend_to(circuit: &BackendCircuit, reg: &mut QuantumRegister) -> Result<(), String> {
    for gate in &circuit.gates {
        match *gate {
            BackendGate::Rz(q, angle) => reg.apply_rz(q, angle)?,
            BackendGate::Rot(q, angle) => match circuit.backend {
                Backend::TrappedIon => reg.apply_ry(q, angle)?,
                Backend::IbmQ | Backend::Rigetti => reg.apply_rx(q, angle)?,
            },
            BackendGate::Cx(a, b) => reg.apply_cnot(a, b)?,
            BackendGate::Cz(a, b) => reg.apply_controlled_z(a, b)?,
            BackendGate::Rzz(a, b, angle) => reg.apply_rzz(a, b, angle)?,
        }
    }
    Ok(())
}
