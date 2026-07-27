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
