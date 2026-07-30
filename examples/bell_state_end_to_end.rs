//! End-to-end example: a Bell-state QASM source, run through the real
//! pipeline (parse -> optimize_ir -> lower(IbmQ) -> to_ibm_qasm) to
//! produce real IBM-basis QASM, plus a local-simulator shot histogram
//! to use as the reference distribution when comparing against a real
//! hardware run via `submit_ibm.py`.
//!
//! Run with: cargo run --example bell_state_end_to_end
//!
//! (Adjust the crate name below to match your actual Cargo.toml
//! package name if it differs from `sirraya_qutub_transpiler`.)

use sirraya_qutub_transpiler::{emit, lower, optimize_ir, qasm, to_ibm_qasm, Backend};
use std::collections::HashMap;
use std::fs;

const SHOTS: usize = 4096;

fn main() {
    let source = "\
OPENQASM 2.0;
qreg q[2];
creg c[2];
h q[0];
cx q[0], q[1];
measure q[0] -> c[0];
measure q[1] -> c[1];
";

    let circuit = qasm::parse(source).expect("parse failed");
    let circuit = optimize_ir(&circuit);
    let backend_circuit = lower(&circuit, Backend::IbmQ);

    let qasm_text = to_ibm_qasm(&backend_circuit, "bell_state").expect("IBM export failed");
    fs::write("bell.qasm", &qasm_text).expect("failed to write bell.qasm");
    println!("Wrote bell.qasm:\n{}", qasm_text);

    // Reference distribution from the simulator. measure_single_qubit
    // collapses the register on each call, so there's no batched
    // "shots" primitive on QuantumRegister itself -- each shot re-runs
    // the whole circuit from a fresh |00> state.
    let mut counts: HashMap<String, u64> = HashMap::new();
    for _ in 0..SHOTS {
        let (_, clbits) =
            emit::run_backend_with_measurement(&backend_circuit).expect("simulator run failed");
        let bitstring: String = clbits
            .iter()
            .rev()
            .map(|b| if *b == 0 { '0' } else { '1' })
            .collect();
        *counts.entry(bitstring).or_insert(0) += 1;
    }

    println!("\nSimulator reference distribution ({} shots):", SHOTS);
    for (bitstring, count) in &counts {
        println!("  {bitstring}: {count}");
    }

    let json = counts_to_json(&counts);
    fs::write("bell_reference_counts.json", &json).expect("failed to write reference counts");
    println!("\nWrote bell_reference_counts.json -- compare against a real run with:");
    println!(
        "  python3 submit_ibm.py --qasm bell.qasm --real --backend <name> \\\n    --compare bell_reference_counts.json"
    );
}

/// Hand-rolled JSON object serialization -- avoids pulling in serde_json
/// just for this one example. Keys are QASM-order bitstrings (already
/// plain ASCII '0'/'1'), so no escaping is needed.
fn counts_to_json(counts: &HashMap<String, u64>) -> String {
    let mut entries: Vec<String> = counts
        .iter()
        .map(|(bitstring, count)| format!("  \"{}\": {}", bitstring, count))
        .collect();
    entries.sort();
    format!("{{\n{}\n}}\n", entries.join(",\n"))
}
