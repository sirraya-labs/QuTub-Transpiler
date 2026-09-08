//! Run one complete correction round with each three-qubit repetition code.
//!
//! Run with: `cargo run --example qec_three_qubit_codes`

use sirraya_qutub::core::QuantumRegister;
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::qec::bit_flip::ThreeQubitBitFlipCode;
use sirraya_qutub_transpiler::qec::phase_flip::ThreeQubitPhaseFlipCode;
use sirraya_qutub_transpiler::qec::StabilizerCode;
use sirraya_qutub_transpiler::{decompose, emit, optimize_ir};

const DATA_QUBITS: [usize; 3] = [0, 1, 2];
const ANCILLA_QUBITS: [usize; 2] = [3, 4];
const SYNDROME_CLBITS: [usize; 2] = [0, 1];

fn run_round(
    code: &dyn StabilizerCode,
    injected_error: Gate,
    expected_syndrome: [u8; 2],
) -> Result<(), String> {
    let mut circuit = Circuit::new(DATA_QUBITS.len() + ANCILLA_QUBITS.len());
    circuit.num_clbits = SYNDROME_CLBITS.len();

    // Prepare logical |1>, encode it across q0..q2, then corrupt the middle
    // physical qubit. Both codes map a q1 error to syndrome bits (1, 1).
    circuit.push(Gate::X(DATA_QUBITS[0]));
    code.encode(&mut circuit, &DATA_QUBITS);
    circuit.push(injected_error.clone());
    code.extract_syndrome(
        &mut circuit,
        &DATA_QUBITS,
        &ANCILLA_QUBITS,
        &SYNDROME_CLBITS,
    );

    let correction_start = circuit.gates.len();
    code.correct(&mut circuit, &DATA_QUBITS, &SYNDROME_CLBITS);
    let correction_end = circuit.gates.len();

    // correct() appends one Gate::If branch for each non-zero syndrome. At
    // runtime only the branch matching the measured (1, 1) syndrome executes.
    if !circuit.gates[correction_start..correction_end]
        .iter()
        .all(|gate| matches!(gate, Gate::If(..)))
    {
        return Err(format!(
            "{} emitted a non-conditional correction",
            code.id()
        ));
    }

    code.decode(&mut circuit, &DATA_QUBITS);

    let native = decompose(&optimize_ir(&circuit));
    let (register, syndrome) = emit::run_with_measurement(&native)?;

    if syndrome != expected_syndrome {
        return Err(format!(
            "{} measured unexpected syndrome {syndrome:?}",
            code.id()
        ));
    }

    let recovered = register
        .to_density_matrix()?
        .partial_trace(&[DATA_QUBITS[0]])?;
    let mut logical_one = QuantumRegister::new(1)?;
    logical_one.apply_pauli_x(0)?;
    let target = logical_one.to_density_matrix()?;
    let fidelity = recovered.fidelity(&target)?;

    if (fidelity - 1.0).abs() > 1e-9 {
        return Err(format!(
            "{} recovered logical |1> with fidelity {fidelity}",
            code.id()
        ));
    }

    println!("{}", code.id());
    println!("  injected error: {injected_error:?}");
    println!("  measured syndrome: {syndrome:?}");
    println!("  conditional correction table:");
    for correction in &circuit.gates[correction_start..correction_end] {
        println!("    {correction:?}");
    }
    println!("  recovered logical |1> fidelity: {fidelity:.6}\n");

    Ok(())
}

fn main() -> Result<(), String> {
    run_round(&ThreeQubitBitFlipCode, Gate::X(DATA_QUBITS[1]), [1, 1])?;
    run_round(&ThreeQubitPhaseFlipCode, Gate::Z(DATA_QUBITS[1]), [1, 1])?;

    Ok(())
}
