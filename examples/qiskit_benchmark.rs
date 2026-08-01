//! Head-to-head comparison against Qiskit's own transpiler, on the
//! same source circuits, targeting the same real basis gate set
//! (`{rz, sx, x, cx}`, IBM's actual hardware-native gates -- see
//! `ibm_export.rs`'s module doc). Self-reported numbers only prove
//! internal consistency; a number that survives being checked against
//! the industry-standard tool is what actually earns trust.
//!
//! This side (Rust) runs each benchmark circuit through the real
//! pipeline -- `qasm::parse` -> `optimize_ir` -> `backend::lower(IbmQ)`
//! -- and reports gate counts, a depth estimate, and a fidelity
//! estimate against IBM Heron r2's published calibration. It also
//! writes each benchmark's *source*-level QASM (standard `h`/`rz`/
//! `ry`/`rx`/`cx`, portable to any QASM 2.0 reader) to disk.
//!
//! The companion `qiskit_transpile_compare.py` loads those same source
//! files and runs `qiskit.transpile(..., basis_gates=["rz","sx","x","cx"],
//! optimization_level=3)` -- the same target basis this crate's own
//! `ibm_export::to_ibm_qasm` produces -- and reports Qiskit's own gate
//! counts and depth for the identical circuits, so the two tables can
//! be read side by side.
//!
//! Run with: `cargo run --example qiskit_benchmark`
//! Then:     `python3 qiskit_transpile_compare.py`
//! (requires `pip install qiskit`)

use sirraya_qutub_transpiler::coupling::CouplingMap;
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::{fidelity, optimize_ir, qasm, lower, Backend};
use std::fs;

/// Standard GHZ-state preparation: `H` on qubit 0, then a `Cx` ladder
/// out to every other qubit. A staple correctness/scaling benchmark
/// (also used in `fidelity_scaling.rs`) precisely because its
/// structure is trivial to reason about by hand.
fn ghz(num_qubits: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    c.push(Gate::H(0));
    for q in 1..num_qubits {
        c.push(Gate::Cx(0, q));
    }
    c
}

/// A hardware-efficient ansatz: `layers` repetitions of (a layer of
/// per-qubit `Ry` rotations, then a linear `Cx` entangling ladder),
/// finished with one more rotation layer -- the same circuit shape
/// QAOA/VQE benchmarks (and MQT Bench / QASMBench's own "twolocal"
/// entries) use. Angles are a fixed deterministic sequence, not
/// random, so this example's output is reproducible run to run.
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

/// A "quantum-volume-lite" circuit: alternating layers of per-qubit
/// single-qubit rotations (mixing `Rx`/`Ry`/`Rz`/`H`) and a full
/// nearest-neighbor `Cx` ladder, several rounds deep -- denser and
/// less structured than the ansatz above, closer to what a randomized
/// benchmarking circuit looks like, while staying deterministic.
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
        let offset = round % 2; // alternate ladder parity, like a brick-wall circuit
        let mut q = offset;
        while q + 1 < num_qubits {
            c.push(Gate::Cx(q, q + 1));
            q += 2;
        }
    }
    c
}

/// Serializes a source-level [`Circuit`] to portable OPENQASM 2.0 text
/// using only broadly-supported `qelib1.inc` mnemonics (`h`, `x`, `y`,
/// `z`, `rx`, `ry`, `rz`, `cx`, `cz`, `swap`) -- a circuit built this
/// way round-trips through both this crate's own [`qasm::parse`] *and*
/// Qiskit's `QuantumCircuit.from_qasm_str`, which is the whole point:
/// the Rust and Python sides need to start from the exact same
/// circuit, not two independently-described approximations of it.
/// Not part of the crate's public API -- this is example-local, since
/// nothing in the library needs a source-level `Circuit -> QASM`
/// writer for its own pipeline (only `emit::to_qasm`/`ibm_export::to_ibm_qasm`,
/// which serialize the *native*/*IBM-basis* gate sets, not this one).
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
            other => panic!(
                "circuit_to_portable_qasm: {:?} isn't in this example's portable subset \
                 -- benchmark circuits here are deliberately built from h/x/y/z/rx/ry/rz/cx/cz/swap only",
                other
            ),
        }
    }
    out
}

/// Same critical-path depth estimate used by `routing_demo.rs`: the
/// longest chain of gates touching any single qubit.
fn depth(c: &Circuit) -> usize {
    let mut last_layer = vec![0usize; c.num_qubits];
    for gate in &c.gates {
        let qs = gate.qubits();
        let layer = qs.iter().map(|&q| last_layer[q]).max().unwrap_or(0) + 1;
        for q in qs {
            last_layer[q] = layer;
        }
    }
    last_layer.into_iter().max().unwrap_or(0)
}

/// Writes `coupling`'s adjacency as `i j` edge lines, one per line --
/// the exact same heavy-hex topology `backend::lower(Backend::IbmQ)`
/// routed this benchmark against (see `coupling.rs`'s module doc:
/// `CouplingMap::heavy_hex_for` is what `Backend::IbmQ`'s own
/// `coupling_map` uses). Read back by `qiskit_transpile_compare.py`
/// to build an identical Qiskit `CouplingMap`, so the two sides are
/// solving the same connectivity-constrained routing problem instead
/// of Qiskit getting an unconstrained all-to-all target while this
/// crate pays real SWAP overhead -- that mismatch, not transpiler
/// quality, is what an unconstrained comparison would actually be
/// measuring.
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
        ("ansatz_6q_3layer", hardware_efficient_ansatz(6, 3)),
        ("layered_random_8q_4round", layered_random(8, 4)),
    ];

    fs::create_dir_all("qiskit_benchmark_qasm").expect("failed to create output dir");

    println!(
        "{:<28}  {:>10}  {:>6}  {:>10}  {:>10}  {:>12}",
        "benchmark", "src gates", "depth", "1q (IBM)", "2q (IBM)", "est fidelity"
    );

    for (name, circuit) in &benchmarks {
        // Round-trip sanity: this crate's own parser must accept the
        // exact QASM text Qiskit will also be asked to load, so the
        // two sides are provably looking at the same circuit.
        let qasm_text = circuit_to_portable_qasm(circuit, name);
        let path = format!("qiskit_benchmark_qasm/{}.qasm", name);
        fs::write(&path, &qasm_text).expect("failed to write QASM");
        let reparsed = qasm::parse(&qasm_text).expect("this crate's own QASM must round-trip");
        assert_eq!(reparsed.gates.len(), circuit.gates.len(), "round-trip lost or gained gates");

        // This crate's real pipeline: source-level optimization, then
        // lowered to IBM's actual native basis via backend::lower.
        let optimized = optimize_ir(circuit);
        let bc = lower(&optimized, Backend::IbmQ);
        let (single, two) = bc.gate_counts();
        let cal = fidelity::PublishedCalibration::ibm_heron_r2();
        let est_fidelity = fidelity::estimate_backend_circuit_fidelity(&bc, &cal);

        // The exact heavy-hex topology backend::lower routed this
        // circuit against (see coupling.rs's module doc: heavy_hex_for
        // is what Backend::IbmQ's own coupling_map uses) -- exported
        // so the Python side can constrain Qiskit's transpile() to the
        // identical connectivity instead of an unconstrained target.
        let coupling = CouplingMap::heavy_hex_for(circuit.num_qubits);
        export_coupling_map(&coupling, &format!("qiskit_benchmark_qasm/{}_coupling.txt", name));

        println!(
            "{:<28}  {:>10}  {:>6}  {:>10}  {:>10}  {:>11.6}%",
            name,
            circuit.gates.len(),
            depth(circuit),
            single,
            two,
            est_fidelity * 100.0
        );
    }

    println!(
        "\nWrote source-level QASM for each benchmark to ./qiskit_benchmark_qasm/*.qasm, and \
         the exact heavy-hex coupling map each was routed against to \
         ./qiskit_benchmark_qasm/*_coupling.txt.\n\
         Run `python3 qiskit_transpile_compare.py` to transpile the same circuits with \
         Qiskit against the *identical* connectivity constraint \
         (basis_gates=[\"rz\",\"sx\",\"x\",\"cx\"], optimization_level=3) and print its own \
         gate-count/depth table for a direct, apples-to-apples comparison against the numbers \
         above -- both sides start from the identical QASM source, target the same real IBM \
         basis, and route against the same physical topology."
    );
}