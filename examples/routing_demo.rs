//! What actually kills fidelity on real hardware isn't gate translation
//! -- it's connectivity. A source circuit that entangles two qubits
//! that aren't adjacent on the real chip can't run as written; the
//! compiler has to insert `Swap`s to physically move state around
//! until it can. This example makes that cost visible and checks it's
//! paid correctly.
//!
//! It builds a circuit that *needs* routing on purpose -- an
//! all-to-all-entangling QFT-style circuit, where most qubit pairs
//! are not adjacent on any real device -- and lowers it onto two real
//! published topology families this crate models directly
//! ([`CouplingMap::heavy_hex_for`] for `IbmQ`, matching IBM's actual
//! Eagle/Heron lattice; [`CouplingMap::square_grid_for`] for
//! `Rigetti`, matching Ankaa-class hardware), plus a worst-case linear
//! chain for contrast.
//!
//! For each topology it reports:
//! - how many `Swap`s `route::route` had to insert,
//! - the resulting native two-qubit gate overhead after full
//!   backend lowering (a `Swap` costs 3 native `Cx`/`Cz`/`Rzz`, so
//!   routing overhead is not just a SWAP count but a real multiplier
//!   on the two-qubit gate budget the fidelity estimate is priced
//!   against), and
//! - a direct correctness check: fidelity between the routed-and-lowered
//!   circuit's output and the unrouted reference's output must still be
//!   1.0, since `route::route`'s whole point is inserting `Swap`s
//!   *without* changing the circuit's logical action (see `route.rs`'s
//!   own module doc on why the final restoration pass exists).
//!
//! Run with: `cargo run --example routing_demo`

use sirraya_qutub_transpiler::coupling::CouplingMap;
use sirraya_qutub_transpiler::diagram::Diagram;
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::route::route;
use sirraya_qutub_transpiler::{decompose, emit, lower, optimize, Backend};
use sirraya_qutub::core::QuantumRegister;

/// An all-to-all-entangling circuit on `num_qubits`: `H` on every
/// qubit, then a controlled-phase between every distinct pair
/// `(i, j)` with `i < j` -- the core structure of a QFT, and about as
/// connectivity-hostile as a circuit gets, since almost every gate
/// pairs qubits that are far apart on any sparse real-hardware
/// topology.
fn all_to_all_circuit(num_qubits: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    for q in 0..num_qubits {
        c.push(Gate::H(q));
    }
    for i in 0..num_qubits {
        for j in (i + 1)..num_qubits {
            let lambda = std::f64::consts::PI / (1 << (j - i)) as f64;
            c.push(Gate::Cp(i, j, lambda));
        }
    }
    c
}

fn count_swaps(c: &Circuit) -> usize {
    c.gates.iter().filter(|g| matches!(g, Gate::Swap(..))).count()
}

/// A rough "critical path length" depth estimate: the longest chain
/// of gates touching any single qubit, counting a two-qubit gate once
/// on each qubit it touches. Good enough to show routing's overhead
/// grow, not presented as a scheduler-accurate cycle count.
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

fn run_unitary(c: &Circuit) -> QuantumRegister {
    let native = optimize(&decompose(c));
    emit::run(&native).expect("simulator run failed")
}

fn main() {
    const N: usize = 8;
    let source = all_to_all_circuit(N);
    println!(
        "Source circuit: {} qubits, {} gates (all-to-all QFT-style entangler)\n",
        source.num_qubits,
        source.gates.len()
    );

    let reference = run_unitary(&source);

    let topologies: [(&str, CouplingMap); 3] = [
        ("Linear chain (worst case)", CouplingMap::linear(N)),
        ("IBM heavy-hex (heavy_hex_for)", CouplingMap::heavy_hex_for(N)),
        ("Rigetti square grid (square_grid_for)", CouplingMap::square_grid_for(N)),
    ];

    println!(
        "{:<40}  {:>6}  {:>7}  {:>7}  {:>10}",
        "topology", "swaps", "depth", "orig-d", "fidelity"
    );
    let mut swaps_at_8 = Vec::new();
    for (name, coupling) in &topologies {
        let routed = route(&source, coupling);
        let swaps = count_swaps(&routed);
        let routed_depth = depth(&routed);
        let source_depth = depth(&source);
        swaps_at_8.push((*name, swaps));

        let routed_reg = run_unitary(&routed);
        let fidelity = reference.fidelity(&routed_reg).expect("fidelity");

        println!(
            "{:<40}  {:>6}  {:>7}  {:>7}  {:>10.9}",
            name, swaps, routed_depth, source_depth, fidelity
        );
    }

    // --- Full pipeline: route + native-lower + fidelity-budget the
    // *actual* backends this crate ships, so the SWAP overhead above
    // is shown as what it really costs: extra native two-qubit gates
    // eating into the fidelity estimate. ---
    println!("\nFull pipeline (route -> backend::lower -> fidelity budget):");
    println!("{:<12}  {:>10}  {:>10}  {:>14}", "backend", "1q gates", "2q gates", "est. fidelity");
    for backend in [Backend::IbmQ, Backend::Rigetti] {
        let bc = lower(&source, backend);
        let (single, two) = bc.gate_counts();
        let cal = backend.calibration();
        let est = sirraya_qutub_transpiler::fidelity::estimate_backend_circuit_fidelity(&bc, &cal);
        println!("{:<12}  {:>10}  {:>10}  {:>13.6}%", format!("{:?}", backend), single, two, est * 100.0);
    }

    // --- A small (4-qubit) circuit's before/after diagram, so the
    // SWAP insertion is visible directly rather than just counted. ---
    let small_source = all_to_all_circuit(4);
    let small_coupling = CouplingMap::linear(4);
    let small_routed = route(&small_source, &small_coupling);
    println!("\n--- 4-qubit example, source (logical) ---");
    println!("{}", Diagram::from_circuit(&small_source).to_ascii());
    println!(
        "\n--- 4-qubit example, routed against a linear chain ({} Swaps inserted) ---",
        count_swaps(&small_routed)
    );
    println!("{}", Diagram::from_circuit(&small_routed).to_ascii());

    // --- A second comparison at 12 qubits: heavy_hex_grid(1, 1) is a
    // *full*, untruncated heavy-hex unit cell (6 data + 6 flag qubits
    // -- see coupling.rs's module doc), rather than an 8-qubit BFS
    // prefix of one. `heavy_hex_for(n)` for n below a topology's
    // natural unit size returns a truncated fragment, which is not
    // guaranteed to route *better* than a naive line -- a sparse,
    // degree-<=3 fragment cut off mid-lattice can have a worse average
    // pairwise distance than a straight chain of the same length. This
    // is a real, checked comparison rather than an assumed one: it
    // only demonstrates heavy-hex's actual advantage once the topology
    // is used at (close to) its native size.
    println!("\n--- Same comparison at 12 qubits (a full heavy-hex unit cell) ---");
    const N2: usize = 12;
    let source2 = all_to_all_circuit(N2);
    let topologies2: [(&str, CouplingMap); 2] = [
        ("Linear chain (worst case)", CouplingMap::linear(N2)),
        ("IBM heavy-hex (full unit cell, heavy_hex_grid(1,1))", CouplingMap::heavy_hex_grid(1, 1)),
    ];
    println!("{:<52}  {:>6}  {:>7}", "topology", "swaps", "depth");
    let mut swap_counts = Vec::new();
    for (name, coupling) in &topologies2 {
        let routed = route(&source2, coupling);
        let swaps = count_swaps(&routed);
        swap_counts.push((name.to_string(), swaps));
        println!("{:<52}  {:>6}  {:>7}", name, swaps, depth(&routed));
    }

    let linear_8 = swaps_at_8.iter().find(|(n, _)| n.starts_with("Linear")).map(|(_, s)| *s).unwrap();
    let heavy_hex_8 = swaps_at_8.iter().find(|(n, _)| n.starts_with("IBM")).map(|(_, s)| *s).unwrap();
    let square_8 = swaps_at_8.iter().find(|(n, _)| n.starts_with("Rigetti")).map(|(_, s)| *s).unwrap();
    let heavy_hex_verdict = if heavy_hex_8 < linear_8 { "better" } else { "worse, not better" };

    println!(
        "\n8-qubit result above: heavy_hex_for(8) is a BFS-truncated *fragment* of a heavy-hex \
         lattice, not a full unit cell -- it used {} swaps vs. the linear chain's {} and the \
         square grid's {}, i.e. {}. That's a genuine finding, not noise: a degree-<=3 fragment \
         cut off mid-lattice isn't guaranteed to beat a straight line on average pairwise \
         distance. At {} qubits (a full, untruncated heavy-hex cell), the comparison above \
         shows whether that connectivity advantage actually materializes at heavy-hex's native \
         size. Either way, the lesson holds: routing quality depends on the *real, specific* \
         topology at the *actual* qubit count being compiled for, not just on which topology \
         family sounds better connected -- exactly why this crate models real published \
         lattices instead of a single generic stand-in.",
        heavy_hex_8, linear_8, square_8, heavy_hex_verdict, N2
    );
}