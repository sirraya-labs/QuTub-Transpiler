//! Head-to-head comparison against Qiskit's own transpiler, on the
//! same source circuits, targeting the same real basis gate set
//! (`{rz, sx, x, cx}`, IBM's actual hardware-native gates).
//!
//! Run with: `cargo run --example qiskit_benchmark`
//! Then:     `python3 qiskit_transpile_compare.py`

use sirraya_qutub_transpiler::coupling::CouplingMap;
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::{fidelity, optimize_ir, qasm, lower, Backend, BackendCircuit, BackendGate};
use std::fs;

fn ghz(num_qubits: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    c.push(Gate::H(0));
    for q in 0..num_qubits.saturating_sub(1) {
        c.push(Gate::Cx(q, q + 1));
    }
    c
}

fn hardware_efficient_ansatz(num_qubits: usize, layers: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    let mut angle_seed = 0.0_f64;
    let mut next_angle = || {
        angle_seed += 0.37;
        angle_seed % std::f64::consts::TAU
    };
    for _ in 0..layers {
        for q in 0..num_qubits {
            c.push(Gate::Ry(q, next_angle()));
        }
        for q in 0..num_qubits.saturating_sub(1) {
            c.push(Gate::Cx(q, q + 1));
        }
    }
    for q in 0..num_qubits {
        c.push(Gate::Ry(q, next_angle()));
    }
    c
}

fn layered_random(num_qubits: usize, rounds: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    let mut seed = 0.0_f64;
    let mut next_angle = || {
        seed += 0.71;
        seed % std::f64::consts::TAU
    };
    for round in 0..rounds {
        for q in 0..num_qubits {
            match (q + round) % 4 {
                0 => c.push(Gate::Rx(q, next_angle())),
                1 => c.push(Gate::Ry(q, next_angle())),
                2 => c.push(Gate::Rz(q, next_angle())),
                _ => c.push(Gate::H(q)),
            };
        }
        let offset = round % 2;
        let mut q = offset;
        while q + 1 < num_qubits {
            c.push(Gate::Cx(q, q + 1));
            q += 2;
        }
    }
    c
}

fn qft(num_qubits: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    for i in 0..num_qubits {
        c.push(Gate::H(i));
        for j in (i + 1)..num_qubits {
            let lambda = std::f64::consts::PI / (1u64 << (j - i)) as f64;
            c.push(Gate::Cp(j, i, lambda));
        }
    }
    for q in 0..num_qubits / 2 {
        c.push(Gate::Swap(q, num_qubits - 1 - q));
    }
    c
}

fn long_range_random(num_qubits: usize, num_gates: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    for q in 0..num_qubits {
        c.push(Gate::H(q));
    }
    let mut state: u64 = 0x2545F4914F6CDD1D;
    let mut next_u64 = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for i in 0..num_gates {
        let a = (next_u64() as usize) % num_qubits;
        let mut b = (next_u64() as usize) % num_qubits;
        while b == a {
            b = (next_u64() as usize) % num_qubits;
        }
        c.push(Gate::Cx(a, b));
        if i % 3 == 0 {
            c.push(Gate::Rz((next_u64() as usize) % num_qubits, 0.4 + (i as f64) * 0.017));
        }
    }
    c
}

fn circuit_to_portable_qasm(c: &Circuit, name: &str) -> String {
    let mut out = String::new();
    out.push_str("OPENQASM 2.0;\n");
    out.push_str("include \"qelib1.inc\";\n");
    out.push_str(&format!("qreg q[{}];\n", c.num_qubits));
    out.push_str(&format!("// benchmark: {}\n", name));
    for g in &c.gates {
        match g {
            Gate::H(q) => out.push_str(&format!("h q[{}];\n", q)),
            Gate::X(q) => out.push_str(&format!("x q[{}];\n", q)),
            Gate::Y(q) => out.push_str(&format!("y q[{}];\n", q)),
            Gate::Z(q) => out.push_str(&format!("z q[{}];\n", q)),
            Gate::Rx(q, a) => out.push_str(&format!("rx({}) q[{}];\n", a, q)),
            Gate::Ry(q, a) => out.push_str(&format!("ry({}) q[{}];\n", a, q)),
            Gate::Rz(q, a) => out.push_str(&format!("rz({}) q[{}];\n", a, q)),
            Gate::Cx(a, b) => out.push_str(&format!("cx q[{}], q[{}];\n", a, b)),
            Gate::Cz(a, b) => out.push_str(&format!("cz q[{}], q[{}];\n", a, b)),
            Gate::Swap(a, b) => out.push_str(&format!("swap q[{}], q[{}];\n", a, b)),
            Gate::Cp(a, b, lambda) => out.push_str(&format!("cp({}) q[{}], q[{}];\n", lambda, a, b)),
            other => panic!("Unsupported gate: {:?}", other),
        }
    }
    out
}

fn backend_depth(c: &BackendCircuit) -> usize {
    let mut last_layer = vec![0usize; c.num_qubits];
    for gate in &c.gates {
        let qs: Vec<usize> = match *gate {
            BackendGate::Rz(q, _) | BackendGate::Rot(q, _) | BackendGate::Measure(q, _) => {
                vec![q]
            }
            BackendGate::Cx(a, b) | BackendGate::Cz(a, b) | BackendGate::Rzz(a, b, _) => {
                vec![a, b]
            }
        };
        let layer = qs.iter().map(|&q| last_layer[q]).max().unwrap_or(0) + 1;
        for q in qs {
            last_layer[q] = layer;
        }
    }
    last_layer.into_iter().max().unwrap_or(0)
}

fn export_coupling_map(coupling: &CouplingMap, path: &str) {
    let n = coupling.num_qubits();
    let mut lines = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            if coupling.is_adjacent(i, j) {
                lines.push(format!("{} {}", i, j));
            }
        }
    }
    fs::write(path, lines.join("\n") + "\n").expect("failed to write coupling map");
}

fn main() {
    let benchmarks: Vec<(&str, Circuit)> = vec![
        ("ghz_10", ghz(10)),
        ("ghz_16", ghz(16)),
        ("ansatz_6q_3layer", hardware_efficient_ansatz(6, 3)),
        ("layered_random_8q_4round", layered_random(8, 4)),
        ("qft_10", qft(10)),
        ("qft_16", qft(16)),
        ("long_range_random_20q_60gate", long_range_random(20, 60)),
    ];

    fs::create_dir_all("qiskit_benchmark_qasm").expect("failed to create output dir");

    println!("\n=== SIRRAYA ROUTING RESULTS (WITH QFT OPTIMIZER) ===");
    println!("{:<28}  {:>10}  {:>11}  {:>10}  {:>10}  {:>12}  {:>10}",
        "benchmark", "src gates", "depth (IBM)", "1q (IBM)", "2q (IBM)", "est fidelity", "est swaps");

    for (name, circuit) in &benchmarks {
        let qasm_text = circuit_to_portable_qasm(circuit, name);
        let path = format!("qiskit_benchmark_qasm/{}.qasm", name);
        fs::write(&path, &qasm_text).expect("failed to write QASM");
        let reparsed = qasm::parse(&qasm_text).expect("QASM must round-trip");
        assert_eq!(reparsed.gates.len(), circuit.gates.len(), "round-trip lost or gained gates");

        // Use route_best for optimized routing
        let coupling = CouplingMap::heavy_hex_for(circuit.num_qubits);
        let routed = sirraya_qutub_transpiler::route::route_best(circuit, &coupling);
        
        // Count swaps in the routed circuit
        let swap_count = routed.gates.iter()
            .filter(|g| matches!(g, Gate::Swap(..)))
            .count();

        // Now lower the routed circuit to IBM's native basis
        let optimized = optimize_ir(&routed);
        let bc = lower(&optimized, Backend::IbmQ);
        let (single, two) = bc.gate_counts();
        let cal = fidelity::PublishedCalibration::ibm_heron_r2();
        let est_fidelity = fidelity::estimate_backend_circuit_fidelity(&bc, &cal);

        export_coupling_map(&coupling, &format!("qiskit_benchmark_qasm/{}_coupling.txt", name));

        println!(
            "{:<28}  {:>10}  {:>11}  {:>10}  {:>10}  {:>11.6}%  {:>10}",
            name,
            circuit.gates.len(),
            backend_depth(&bc),
            single,
            two,
            est_fidelity * 100.0,
            swap_count
        );
    }

    println!("\n=== QISKIT COMPARISON ===");
    println!("Run: python3 qiskit_transpile_compare.py");
    println!("to see Qiskit's numbers for the same circuits.");
    println!("\nWrote source-level QASM to ./qiskit_benchmark_qasm/*.qasm");
    println!("and coupling maps to ./qiskit_benchmark_qasm/*_coupling.txt");
}