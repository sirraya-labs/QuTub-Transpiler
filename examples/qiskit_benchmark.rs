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
use sirraya_qutub_transpiler::{fidelity, optimize_ir, qasm, lower, Backend, BackendCircuit, BackendGate};
use std::fs;

/// **Topology caveat (why this benchmark table includes both `ghz_10`
/// and `ghz_16`):** `CouplingMap::heavy_hex_for(n)` picks the smallest
/// `d x d` hexagon grid with `>= n` qubits, and a single hexagon
/// (`d=1`) already has 12 qubits -- so for any `n <= 12`,
/// `heavy_hex_for(n)` is a plain 12-cycle with the tail truncated, max
/// degree 2 everywhere. Heavy-hex's actual defining feature (interior
/// degree-3 "branch" qubits, from two hexagons sharing a wall) doesn't
/// exist in the coupling map at all until `n >= 13`. `ghz_10` is a
/// real, useful regression case (see `route.rs`'s
/// `ghz_chain_routes_with_fewer_swaps_via_chain_fast_path_on_heavy_hex`
/// test, which target-picks a similarly small `n` on purpose), but a
/// benchmark table that only ever routes against `n <= 12` is silently
/// never exercising heavy-hex's actual branching structure, degree-3
/// nodes included -- `ghz_16` below is included specifically to fix
/// that gap for this comparison table.
///
/// Standard GHZ-state preparation, built as a **linear CNOT chain**:
/// `H` on qubit 0, then `Cx(q, q+1)` for every adjacent pair. Produces
/// the exact same target state as the more commonly-seen "star"
/// construction (`H(0)` then `Cx(0, q)` fanning out from a single hub
/// qubit for every other `q`) -- but the two have completely different
/// interaction graphs, and that difference is the whole reason this
/// function builds the chain, not the star.
///
/// # Why not the star
/// This function used to build the star. That's a legitimate GHZ
/// circuit, but a bad benchmark choice for *this* crate specifically:
/// `route.rs`'s `choose_initial_layout` has a dedicated "chain fast
/// path" (`detect_interaction_chain` -> `find_hamiltonian_path`) built
/// around exactly the linear-chain interaction shape below -- its own
/// test suite has a test proving the star shape is *rejected* by that
/// detector (`detect_interaction_chain_rejects_a_star`, in
/// `route.rs`), on the grounds that a degree-`(n-1)` hub qubit isn't a
/// chain at all. A star-shaped GHZ never gets to use the fast path
/// this crate actually built for GHZ-shaped circuits, and separately,
/// no numbering or layout choice can route a degree-`(n-1)` hub onto a
/// max-degree-3 heavy-hex qubit for free regardless -- that's a
/// structural mismatch between the circuit and the topology, not
/// something a routing pass can optimize around. Benchmarking the star
/// was measuring that structural mismatch, not this crate's router.
/// The chain construction is also the standard hardware-efficient way
/// to prepare a GHZ state on any bounded-degree device in practice
/// (every qubit here needs at most 2 neighbors, matching heavy-hex's
/// minimum degree) -- not a construction chosen to flatter this
/// benchmark.
fn ghz(num_qubits: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    c.push(Gate::H(0));
    for q in 0..num_qubits.saturating_sub(1) {
        c.push(Gate::Cx(q, q + 1));
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

/// The Quantum Fourier Transform: for each qubit `i` (0-indexed, in the
/// usual "top qubit first" convention), `H(i)` followed by a controlled
/// phase `Cp(j, i, pi / 2^(j-i))` from every later qubit `j`, then a
/// final reversal via `n/2` `Swap`s. Unlike every circuit above, QFT's
/// interaction graph is genuinely **all-to-all** -- qubit 0 interacts
/// with all of qubits `1..n`, qubit 1 with all of `2..n`, and so on --
/// not a chain, not a nearest-neighbor ladder, and not detectable by
/// `route.rs`'s chain fast path (see `detect_interaction_chain`'s own
/// star-rejection test). A real, standard algorithm's circuit, not a
/// synthetic stress case invented to make routing look hard.
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

/// `num_gates` two-qubit gates, each between a genuinely far-apart
/// pair of qubits chosen by a small deterministic LCG (not just
/// adjacent or near-adjacent pairs the way every ladder/brick-wall
/// circuit above stays close to) -- every other benchmark in this file
/// only ever needs nearest-neighbor interactions, which is exactly the
/// case `heavy_hex_for`'s DFS-numbered identity mapping now handles
/// for free (see `coupling.rs`'s module doc); this is the case that
/// actually forces real routing distance regardless of numbering, and
/// so is the honest test of `route_lookahead`'s general (non-chain)
/// heuristic rather than the identity-biased fast path.
fn long_range_random(num_qubits: usize, num_gates: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    for q in 0..num_qubits {
        c.push(Gate::H(q));
    }
    // A simple linear congruential generator, deterministic and with
    // no external `rand` dependency -- this example only needs
    // "spread out and not visibly patterned", not real randomness.
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
/// using only broadly-supported `qelib1.inc` mnemonics (`h`, `x`, `y`,
/// `z`, `rx`, `ry`, `rz`, `cx`, `cz`, `swap`, `cp`) -- a circuit built this
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
            Gate::Cp(a, b, lambda) => out.push_str(&format!("cp({}) q[{}], q[{}];\n", lambda, a, b)),
            other => panic!(
                "circuit_to_portable_qasm: {:?} isn't in this example's portable subset \
                 -- benchmark circuits here are deliberately built from \
                 h/x/y/z/rx/ry/rz/cx/cz/swap/cp only",
                other
            ),
        }
    }
    out
}

/// Same critical-path depth estimate used by `routing_demo.rs`: the
/// longest chain of gates touching any single qubit. Kept for
/// reference/debugging on the *source*-level circuit, but **not** what
/// gets printed in this benchmark's table -- see [`backend_depth`]'s
/// doc comment for why the source-level number isn't the one to
/// compare against Qiskit's.
#[allow(dead_code)]
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

/// The same critical-path depth estimate as [`depth`], but over a
/// lowered, routed [`sirraya_qutub_transpiler::backend::BackendCircuit`]
/// instead of the source-level [`Circuit`] -- **this** is the number
/// this benchmark's table actually prints and compares against
/// Qiskit's own depth column.
///
/// This distinction matters and used to be silently wrong: `main`
/// below builds `bc` via `optimize_ir` -> `lower(_, Backend::IbmQ)`
/// (which routes against heavy-hex, inserting `Swap`s, and expands
/// every gate to the `{rz, sx/rot, cx}` native basis), but the printed
/// "depth" column was computed by calling `depth(circuit)` on the
/// original, un-routed, un-lowered *source* circuit instead of on
/// `bc`. Qiskit's own depth column, by contrast, is measured on its
/// fully transpiled output circuit -- so the two numbers were never
/// actually describing the same thing (e.g. `ghz_16`'s old printed
/// depth of `16` was the depth of a 16-gate abstract GHZ ladder, not
/// of the 417-gate circuit this crate actually routed and lowered).
/// [`BackendGate::qubits`] doesn't exist as a public helper the way
/// [`Gate::qubits`] does, so this reimplements the same one-line
/// qubit-extraction match `depth` above uses, specialized to
/// `BackendGate`'s variants (`Rz`/`Rot`/`Measure` touch one qubit;
/// `Cx`/`Cz`/`Rzz` touch two).
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
        ("ghz_16", ghz(16)),
        ("ansatz_6q_3layer", hardware_efficient_ansatz(6, 3)),
        ("layered_random_8q_4round", layered_random(8, 4)),
        ("qft_10", qft(10)),
        ("qft_16", qft(16)),
        ("long_range_random_20q_60gate", long_range_random(20, 60)),
    ];

    fs::create_dir_all("qiskit_benchmark_qasm").expect("failed to create output dir");

    println!(
        "{:<28}  {:>10}  {:>11}  {:>10}  {:>10}  {:>12}",
        "benchmark", "src gates", "depth (IBM)", "1q (IBM)", "2q (IBM)", "est fidelity"
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
            "{:<28}  {:>10}  {:>11}  {:>10}  {:>10}  {:>11.6}%",
            name,
            circuit.gates.len(),
            backend_depth(&bc),
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