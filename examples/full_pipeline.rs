//! Full pipeline demo: QASM text in, native `{Rz,Ry,Rzz}` circuit out,
//! a fidelity budget against published Quantinuum Helios figures, and
//! an actual run against `sirraya_qutub::core::QuantumRegister`.
//!
//! Run with: `cargo run --example full_pipeline`

use sirraya_qutub_transpiler::{decompose, emit, estimate_circuit_fidelity, optimize, qasm, PublishedCalibration};

const SOURCE: &str = r#"
    OPENQASM 2.0;
    include "qelib1.inc";
    qreg q[3];
    creg c[3];
    h q[0];
    cx q[0], q[1];
    cx q[1], q[2];
    t q[0];
    ryy(0.9) q[0], q[2];
    swap q[1], q[2];
    cp(0.5) q[0], q[1];
"#;

fn main() -> Result<(), String> {
    let circuit = qasm::parse(SOURCE)?;
    println!(
        "Parsed {} qubits, {} source gates: {:?}",
        circuit.num_qubits,
        circuit.gates.len(),
        circuit.gate_counts()
    );

    let raw_native = decompose(&circuit);
    let native = optimize(&raw_native);
    println!(
        "Native gate set: {} gates before optimization -> {} after",
        raw_native.gates.len(),
        native.gates.len()
    );
    let (single, two) = native.gate_counts();
    println!("  {} single-qubit (Rz/Ry), {} two-qubit (Rzz)", single, two);

    let cal = PublishedCalibration::quantinuum_helios_2026();
    let fidelity_estimate = estimate_circuit_fidelity(&native, &cal);
    println!(
        "Estimated circuit fidelity on {}: {:.6}",
        cal.name, fidelity_estimate
    );

    println!("\nNative QASM:\n{}", emit::to_qasm(&native, "full_pipeline_demo"));

    let reg = emit::run(&native)?;
    println!("Final state probability distribution:");
    let mut dist: Vec<(String, f64)> = reg.get_probability_distribution().into_iter().collect();
    dist.sort_by(|a, b| a.0.cmp(&b.0));
    for (bitstring, prob) in dist {
        if prob > 1e-9 {
            println!("  |{}>: {:.6}", bitstring, prob);
        }
    }

    Ok(())
}
