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

/// Bernstein-Vazirani: recovers a hidden `n`-bit string `secret` in a
/// single oracle query. `num_qubits` includes the ancilla, which is
/// the *last* qubit (index `num_qubits - 1`). Every two-qubit gate in
/// this circuit shares the *same* target (the ancilla) -- a hub/star
/// pattern, not a line -- which turns out to be a genuine stress case
/// for the per-gate greedy router in `route.rs`: each `CX` is routed
/// independently, so nothing carries forward the fact that several
/// gates are all converging on the same physical qubit. On a sparse
/// map like heavy-hex this produces a surprisingly large SWAP count
/// for how few two-qubit gates the source circuit actually has (see
/// `qiskit_benchmark`'s own run output) -- a different kind of
/// worst case for the general routers than QFT's long-range cascade
/// or QAOA's arbitrary-graph edges, worth keeping precisely because
/// it's *not* the same failure mode as those two.
fn bernstein_vazirani(num_qubits: usize, secret: u64) -> Circuit {
    let n = num_qubits - 1;
    let ancilla = n;
    let mut c = Circuit::new(num_qubits);
    c.num_clbits = n;
    c.push(Gate::X(ancilla));
    for q in 0..num_qubits {
        c.push(Gate::H(q));
    }
    for q in 0..n {
        if (secret >> q) & 1 == 1 {
            c.push(Gate::Cx(q, ancilla));
        }
    }
    for q in 0..n {
        c.push(Gate::H(q));
    }
    for q in 0..n {
        c.push(Gate::Measure(q, q));
    }
    c
}

/// Deterministic pseudo-random simple graph on `num_qubits` nodes with
/// exactly `num_edges` distinct undirected edges (same xorshift
/// generator `long_range_random` already uses below, seeded off
/// `num_qubits` so different sizes don't collide). Used as the MaxCut
/// instance for [`qaoa_maxcut`].
fn random_regular_like_graph(num_qubits: usize, num_edges: usize) -> Vec<(usize, usize)> {
    let mut edges = std::collections::BTreeSet::new();
    let mut state: u64 = 0x9E3779B97F4A7C15 ^ (num_qubits as u64).wrapping_mul(0xBF58476D1CE4E5B9);
    let mut next_u64 = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let max_possible = num_qubits * num_qubits.saturating_sub(1) / 2;
    let target = num_edges.min(max_possible);
    while edges.len() < target {
        let a = (next_u64() as usize) % num_qubits;
        let mut b = (next_u64() as usize) % num_qubits;
        while b == a {
            b = (next_u64() as usize) % num_qubits;
        }
        edges.insert(if a < b { (a, b) } else { (b, a) });
    }
    edges.into_iter().collect()
}

/// QAOA for MaxCut, `layers` rounds of the standard cost/mixer
/// alternation: `exp(-i*gamma*Z_a*Z_b)` (== `Rzz(a,b,2*gamma)`) per
/// graph edge, then `exp(-i*beta*X_q)` (== `Rx(q,2*beta)`) per qubit.
/// This is the near-term algorithm benchmark most transpiler
/// comparisons lead with -- unlike QFT's fixed cascade shape, a
/// MaxCut graph's edges are generic non-adjacent pairs, so this
/// exercises the *general-purpose* routers (`route`/`route_lookahead`/
/// `route_sabre`), not `route_qft`'s dedicated fast path.
fn qaoa_maxcut(num_qubits: usize, edges: &[(usize, usize)], layers: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    c.num_clbits = num_qubits;
    for q in 0..num_qubits {
        c.push(Gate::H(q));
    }
    let mut seed = 0.0_f64;
    let mut next_angle = |scale: f64| {
        seed += 0.53;
        scale * (seed % std::f64::consts::TAU)
    };
    for _ in 0..layers {
        let gamma = next_angle(0.35);
        for &(a, b) in edges {
            c.push(Gate::Rzz(a, b, 2.0 * gamma));
        }
        let beta = next_angle(0.25);
        for q in 0..num_qubits {
            c.push(Gate::Rx(q, 2.0 * beta));
        }
    }
    for q in 0..num_qubits {
        c.push(Gate::Measure(q, q));
    }
    c
}

/// A first-order Trotterization of a transverse-field Ising chain,
/// `H = J * sum_i Z_i Z_{i+1} + h * sum_i X_i`, split into even/odd
/// nearest-neighbor `Rzz` bond layers (so within a layer no qubit is
/// touched twice, matching how a real Trotter step is scheduled) plus
/// a transverse-field `Rx` layer, repeated `trotter_steps` times. A
/// genuine physics workload, and -- unlike QFT/QAOA -- purely nearest-
/// neighbor, so it's a useful contrast: this should route with few or
/// zero SWAPs on any coupling map with a Hamiltonian path through all
/// qubits, isolating "does the optimizer clean up a physically
/// realistic circuit well" from "does routing handle long-range
/// interactions well".
fn trotterized_ising_chain(num_qubits: usize, trotter_steps: usize) -> Circuit {
    let j_coupling = 0.8_f64;
    let h_field = 0.5_f64;
    let dt = 0.3_f64;
    let mut c = Circuit::new(num_qubits);
    for _ in 0..trotter_steps {
        for q in (0..num_qubits.saturating_sub(1)).step_by(2) {
            c.push(Gate::Rzz(q, q + 1, 2.0 * j_coupling * dt));
        }
        for q in (1..num_qubits.saturating_sub(1)).step_by(2) {
            c.push(Gate::Rzz(q, q + 1, 2.0 * j_coupling * dt));
        }
        for q in 0..num_qubits {
            c.push(Gate::Rx(q, 2.0 * h_field * dt));
        }
    }
    c
}

/// Textbook Quantum Phase Estimation: `counting_qubits` control qubits
/// (0..counting_qubits) plus one target qubit (index
/// `counting_qubits`) prepared in the `|1>` eigenstate of a phase gate
/// `P(theta)`, `theta = 2*pi * numerator / 2^counting_qubits` (an
/// exactly-representable phase, so QPE recovers `numerator` exactly
/// modulo gate noise -- a real correctness property, not just circuit
/// structure). Controlled-`P(2^k * theta)` per counting qubit, then an
/// inverse QFT on the counting register. Deliberately built from the
/// *inverse*-order cascade (descending `i`, `Cp` before each `H`, plus
/// a leading block of controlled-phase gates `detect_qft_cascade`
/// doesn't expect) so this does **not** hit `route_qft`'s fast path --
/// a second, independently-useful data point on the general routers,
/// beyond QAOA's non-local-but-unstructured edges.
fn quantum_phase_estimation(counting_qubits: usize, numerator: u64) -> Circuit {
    let target = counting_qubits;
    let mut c = Circuit::new(counting_qubits + 1);
    c.num_clbits = counting_qubits;
    c.push(Gate::X(target));
    for q in 0..counting_qubits {
        c.push(Gate::H(q));
    }
    for q in 0..counting_qubits {
        // `qft()`'s cascade+swap construction places qubit 0 as the
        // *most*-significant bit of the register it transforms (
        // verified numerically against the standard DFT matrix, not
        // assumed) -- so qubit 0 here must get the largest phase
        // weight, `2^(counting_qubits-1)`, not the smallest.
        let power = 1u64 << (counting_qubits - 1 - q);
        let lambda = std::f64::consts::TAU * (numerator as f64) * (power as f64)
            / (1u64 << counting_qubits) as f64;
        c.push(Gate::Cp(q, target, lambda));
    }
    // `qft()` below is cascade-then-swap (time order), so its exact
    // inverse is swap-then-inverse-cascade (matrices: QFT = S.C, so
    // QFT^-1 = C^-1.S, which in circuit time-order means S is applied
    // *first*, then C^-1) -- the swap network here must come before
    // the reversed H/Cp cascade, not after it.
    for q in 0..counting_qubits / 2 {
        c.push(Gate::Swap(q, counting_qubits - 1 - q));
    }
    for i in (0..counting_qubits).rev() {
        for j in (i + 1..counting_qubits).rev() {
            let lambda = -std::f64::consts::PI / (1u64 << (j - i)) as f64;
            c.push(Gate::Cp(j, i, lambda));
        }
        c.push(Gate::H(i));
    }
    for q in 0..counting_qubits {
        c.push(Gate::Measure(q, q));
    }
    c
}

/// A GHZ-style circuit with a genuine mid-circuit measurement: build
/// entanglement across the first half of the register, measure those
/// qubits *before* the second half is ever touched, then keep
/// entangling the remainder. Most toy transpilers only ever handle
/// measurement as a final, circuit-ending step; this crate's `Measure`
/// support (the P0.1 roadmap item -- see `emit.rs`/`ir.rs`) is real
/// enough to route and optimize *through* one mid-circuit, and this is
/// the benchmark that actually demonstrates that rather than just
/// asserting it in a unit test.
fn ghz_with_midcircuit_measurement(num_qubits: usize) -> Circuit {
    let half = num_qubits / 2;
    let mut c = Circuit::new(num_qubits);
    c.num_clbits = num_qubits;
    c.push(Gate::H(0));
    for q in 0..half.saturating_sub(1) {
        c.push(Gate::Cx(q, q + 1));
    }
    for q in 0..half {
        c.push(Gate::Measure(q, q));
    }
    if half < num_qubits {
        c.push(Gate::H(half));
    }
    for q in half..num_qubits.saturating_sub(1) {
        c.push(Gate::Cx(q, q + 1));
    }
    for q in half..num_qubits {
        c.push(Gate::Measure(q, q));
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
    if c.num_clbits > 0 {
        out.push_str(&format!("creg c[{}];\n", c.num_clbits));
    }
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
            Gate::Rzz(a, b, theta) => out.push_str(&format!("rzz({}) q[{}], q[{}];\n", theta, a, b)),
            Gate::Cp(a, b, lambda) => out.push_str(&format!("cp({}) q[{}], q[{}];\n", lambda, a, b)),
            Gate::Measure(q, cbit) => out.push_str(&format!("measure q[{}] -> c[{}];\n", q, cbit)),
            other => panic!("Unsupported gate: {:?}", other),
        }
    }
    out
}

fn backend_depth(c: &BackendCircuit) -> usize {
    let mut last_layer = vec![0usize; c.num_qubits];
    for gate in &c.gates {
        let qs = backend_gate_qubits(gate);
        let layer = qs.iter().map(|&q| last_layer[q]).max().unwrap_or(0) + 1;
        for q in qs {
            last_layer[q] = layer;
        }
    }
    last_layer.into_iter().max().unwrap_or(0)
}

/// The qubit(s) a `BackendGate` touches. `If` delegates to `inner` --
/// a conditioned gate occupies exactly the wire(s) its inner gate
/// does, same as `ir::Gate::If` does at the source level (see
/// `ir::Gate::qubits`'s doc comment in the crate itself). Written
/// locally rather than calling the crate's own `BackendGate::qubits`
/// helper, since that one is `pub(crate)` and not visible from an
/// example binary.
fn backend_gate_qubits(gate: &BackendGate) -> Vec<usize> {
    match gate {
        BackendGate::Rz(q, _) | BackendGate::Rot(q, _) | BackendGate::Measure(q, _) => vec![*q],
        BackendGate::Cx(a, b) | BackendGate::Cz(a, b) | BackendGate::Rzz(a, b, _) => vec![*a, *b],
        BackendGate::If(_, inner) => backend_gate_qubits(inner),
    }
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
        ("bernstein_vazirani_10", bernstein_vazirani(11, 0b1011010110)),
        ("qaoa_maxcut_8q_p2", qaoa_maxcut(8, &random_regular_like_graph(8, 12), 2)),
        ("qaoa_maxcut_12q_p3", qaoa_maxcut(12, &random_regular_like_graph(12, 18), 3)),
        ("trotter_ising_10q_6step", trotterized_ising_chain(10, 6)),
        ("trotter_ising_16q_10step", trotterized_ising_chain(16, 10)),
        ("qpe_6counting", quantum_phase_estimation(6, 5)),
        ("qpe_10counting", quantum_phase_estimation(10, 41)),
        ("dynamic_midcircuit_measure_10q", ghz_with_midcircuit_measurement(10)),
        ("qft_10", qft(10)),
        ("qft_16", qft(16)),
        ("long_range_random_20q_60gate", long_range_random(20, 60)),
    ];

    fs::create_dir_all("qiskit_benchmark_qasm").expect("failed to create output dir");

    println!("\n=== SIRRAYA ROUTING RESULTS (WITH QFT OPTIMIZER) ===");
    println!("{:<28}  {:>10}  {:>11}  {:>10}  {:>10}  {:>9}  {:>9}",
        "benchmark", "src gates", "depth (IBM)", "1q (IBM)", "2q (IBM)", "fidelity", "swaps");
    let mut total_routing_swaps = 0usize;
    let mut total_restoration_swaps = 0usize;
    let mut total_no_restore_swaps = 0usize;
    let mut total_fidelity_delta_pp = 0.0f64;
    // Restoration/no_restore numbers are printed as a second, separate
    // table below (see restoration_rows) rather than crammed into this
    // row -- the combined single-table version ran 178 characters
    // wide, which wraps (and visually garbles digits across the wrap
    // point) in any terminal narrower than that.
    struct RestorationRow {
        name: String,
        routing_swaps: usize,
        restoration_swaps: usize,
        restoration_pct: f64,
        no_restore_swaps: usize,
        est_fidelity_nr: f64,
        fidelity_delta_pp: f64,
    }
    let mut restoration_rows: Vec<RestorationRow> = Vec::new();

    for (name, circuit) in &benchmarks {
        let qasm_text = circuit_to_portable_qasm(circuit, name);
        let path = format!("qiskit_benchmark_qasm/{}.qasm", name);
        fs::write(&path, &qasm_text).expect("failed to write QASM");
        let reparsed = qasm::parse(&qasm_text).expect("QASM must round-trip");
        assert_eq!(reparsed.gates.len(), circuit.gates.len(), "round-trip lost or gained gates");

        // Use route_best for optimized routing
        let coupling = CouplingMap::heavy_hex_for(circuit.num_qubits);
        let routed = sirraya_qutub_transpiler::route::route_best(circuit, &coupling);
        
        // Count swaps in the routed circuit, split into "routing"
        // (mid-circuit, load-bearing) vs "restoration" (trailing
        // identity-restore block) -- see restoration_swap_count's own
        // doc comment. The restoration fraction is the thing Priority
        // 2 (`skip_restore`) would actually eliminate, so it's what
        // decides whether that lever is worth building.
        let swap_count = routed.gates.iter()
            .filter(|g| matches!(g, Gate::Swap(..)))
            .count();
        let (routing_swaps, restoration_swaps) =
            sirraya_qutub_transpiler::route::restoration_swap_count(&routed);
        debug_assert_eq!(
            routing_swaps + restoration_swaps, swap_count,
            "restoration_swap_count's split must account for every swap route_best emitted"
        );
        total_routing_swaps += routing_swaps;
        total_restoration_swaps += restoration_swaps;
        let restoration_pct = if swap_count > 0 {
            100.0 * restoration_swaps as f64 / swap_count as f64
        } else {
            0.0
        };

        // route_best_no_restore re-selects candidates by routing-swap
        // count rather than total, so this is not necessarily
        // `routing_swaps` above (a different candidate can win once
        // restoration is off the table) -- see its own doc comment.
        // Only valid for a circuit whose result is read off its
        // Measures, not its final layout -- see restoration table's
        // own footnote below.
        let no_restore_routed =
            sirraya_qutub_transpiler::route::route_best_no_restore(circuit, &coupling);
        let no_restore_swaps = no_restore_routed.gates.iter()
            .filter(|g| matches!(g, Gate::Swap(..)))
            .count();
        total_no_restore_swaps += no_restore_swaps;

        // Now lower the routed circuit to IBM's native basis
        let optimized = optimize_ir(&routed);
        let bc = lower(&optimized, Backend::IbmQ);
        let (single, two) = bc.gate_counts();
        let cal = fidelity::PublishedCalibration::ibm_heron_r2();
        let est_fidelity = fidelity::estimate_backend_circuit_fidelity(&bc, &cal);

        // Same lowering pipeline, run on the no_restore circuit instead
        // of guessing the fidelity impact from the swap-count delta --
        // 2-qubit gates dominate estimate_backend_circuit_fidelity's
        // model and every Swap lowers to 3 CX/CZ (see native.rs's
        // Swap -> Cx;Cx;Cx identity), so this is the real number, not
        // an inference from the table's swap columns.
        let optimized_nr = optimize_ir(&no_restore_routed);
        let bc_nr = lower(&optimized_nr, Backend::IbmQ);
        let est_fidelity_nr = fidelity::estimate_backend_circuit_fidelity(&bc_nr, &cal);
        let fidelity_delta_pp = (est_fidelity_nr - est_fidelity) * 100.0;
        total_fidelity_delta_pp += fidelity_delta_pp;

        export_coupling_map(&coupling, &format!("qiskit_benchmark_qasm/{}_coupling.txt", name));

        println!(
            "{:<28}  {:>10}  {:>11}  {:>10}  {:>10}  {:>8.2}%  {:>9}",
            name,
            circuit.gates.len(),
            backend_depth(&bc),
            single,
            two,
            est_fidelity * 100.0,
            swap_count,
        );

        restoration_rows.push(RestorationRow {
            name: name.to_string(),
            routing_swaps,
            restoration_swaps,
            restoration_pct,
            no_restore_swaps,
            est_fidelity_nr,
            fidelity_delta_pp,
        });
    }

    println!("\n=== RESTORATION TAX / route_best_no_restore ===");
    println!("{:<28}  {:>8}  {:>8}  {:>8}  {:>10}  {:>9}  {:>9}",
        "benchmark", "routing", "restore", "restore%", "no_restore", "nr fid%", "Δfid(pp)");
    for r in &restoration_rows {
        println!(
            "{:<28}  {:>8}  {:>8}  {:>7.2}%  {:>10}  {:>8.2}%  {:>+8.4}",
            r.name,
            r.routing_swaps,
            r.restoration_swaps,
            r.restoration_pct,
            r.no_restore_swaps,
            r.est_fidelity_nr * 100.0,
            r.fidelity_delta_pp,
        );
    }
    println!("(no_restore is only valid for circuits whose result is read off Measures, not final qubit layout)");
    println!(
        "Average no_restore fidelity delta: {:+.4} percentage points across {} benchmarks",
        total_fidelity_delta_pp / benchmarks.len() as f64,
        benchmarks.len()
    );

    let total_swaps = total_routing_swaps + total_restoration_swaps;
    let overall_restoration_pct = if total_swaps > 0 {
        100.0 * total_restoration_swaps as f64 / total_swaps as f64
    } else {
        0.0
    };
    println!(
        "\nTotals: {} routing swaps, {} restoration swaps, {:.2}% of all swaps are restoration tax",
        total_routing_swaps, total_restoration_swaps, overall_restoration_pct
    );
    println!(
        "route_best_no_restore totals: {} swaps ({} fewer than route_best's {} total, {:.2}% reduction)",
        total_no_restore_swaps,
        total_swaps.saturating_sub(total_no_restore_swaps),
        total_swaps,
        if total_swaps > 0 {
            100.0 * (total_swaps.saturating_sub(total_no_restore_swaps)) as f64 / total_swaps as f64
        } else {
            0.0
        }
    );

    println!("\n=== QISKIT COMPARISON ===");
    println!("Run: python3 qiskit_transpile_compare.py");
    println!("to see Qiskit's numbers for the same circuits.");
    println!("\nWrote source-level QASM to ./qiskit_benchmark_qasm/*.qasm");
    println!("and coupling maps to ./qiskit_benchmark_qasm/*_coupling.txt");
}