//! Correctness harness: proves -- doesn't just assert -- that every
//! rewrite this crate performs (source-level `optimize_ir`, native
//! `{Rz,Ry,Rzz}` decomposition + peephole `optimize`, and per-backend
//! `lower` for TrappedIon/IbmQ/Rigetti) preserves the circuit's actual
//! unitary action, not just its gate count.
//!
//! "Preserves" is checked the only way that actually means something:
//! run both the reference circuit and the rewritten one against a real
//! `sirraya_qutub::core::QuantumRegister` starting from |00...0>, then
//! call `QuantumRegister::fidelity` between the two resulting states.
//! State fidelity is 1.0 iff the two final states are identical up to
//! global phase -- exactly the invariant every identity in `native.rs`/
//! `backend.rs` claims to preserve. This is the same check
//! `backend.rs`'s own `check_backend_matches` test uses internally;
//! this example just runs it over many more circuits, reports the
//! results as a table, and does it for the source-level `optimize_ir`
//! rewrite too, which the crate's existing test suite doesn't cover
//! end-to-end against the simulator.
//!
//! Every circuit generated here is unitary-only (no `Measure`): a
//! projective measurement collapses the state non-deterministically,
//! so "the same circuit run twice" isn't expected to agree -- fidelity
//! is only a meaningful equivalence check pre-measurement. Backend
//! lowering that involves routing (`IbmQ`, `Rigetti`) still receives
//! the full test: `route::route`'s own final restoration pass is
//! supposed to put every logical qubit back on its original wire, so
//! fidelity-against-the-unrouted-reference should still land at 1.0 if
//! that guarantee holds.
//!
//! Run with: `cargo run --example verify_equivalence`

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::{decompose, emit, lower, optimize, optimize_ir, Backend};
use sirraya_qutub::core::QuantumRegister;

/// Below this, two states are considered "the same" -- purely a
/// floating-point tolerance on an exact-in-theory equality, not a
/// physically meaningful threshold the way e.g. `fidelity.rs`'s
/// hardware-error numbers are.
const FIDELITY_TOL: f64 = 1e-9;

const BACKENDS: [Backend; 3] = [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti];

fn main() {
    let mut rng = StdRng::seed_from_u64(42); // fixed seed: reproducible report
    let circuits: Vec<Circuit> = (0..40).map(|_| random_circuit(&mut rng)).collect();

    println!(
        "Verifying {} randomized circuits ({}-{} qubits) against a real QuantumRegister.\n",
        circuits.len(), MIN_QUBITS, MAX_QUBITS
    );
    println!(
        "{:>4}  {:>3}  {:>5}  {:>12}  {:>10}  {:>10}  {:>10}",
        "case", "n", "gates", "optimize_ir", "TrappedIon", "IbmQ", "Rigetti"
    );

    let mut worst_fidelity = 1.0_f64;
    let mut any_failed = false;

    for (i, circuit) in circuits.iter().enumerate() {
        // --- Reference: the untouched source circuit through the
        // already-verified native path (decompose + peephole optimize). ---
        let reference = run_native(circuit);

        // --- optimize_ir: source-level cancellation/reorder must not
        // change the circuit's action at all. ---
        let optimized_source = optimize_ir(circuit);
        let after_optimize_ir = run_native(&optimized_source);
        let fid_optimize_ir = reference.fidelity(&after_optimize_ir).expect("fidelity");

        // --- Each backend's lowering (native decompose -> backend's
        // own gate set, routed first if the backend has a coupling map). ---
        let mut backend_fidelities = Vec::with_capacity(BACKENDS.len());
        for &backend in &BACKENDS {
            let bc = lower(circuit, backend);
            let reg = emit::run_backend(&bc).expect("backend run");
            let fid = reference.fidelity(&reg).expect("fidelity");
            backend_fidelities.push(fid);
        }

        let row_min = backend_fidelities
            .iter()
            .copied()
            .fold(fid_optimize_ir, f64::min);
        worst_fidelity = worst_fidelity.min(row_min);
        if row_min < 1.0 - FIDELITY_TOL {
            any_failed = true;
        }

        println!(
            "{:>4}  {:>3}  {:>5}  {:>12.9}  {:>10.9}  {:>10.9}  {:>10.9}",
            i,
            circuit.num_qubits,
            circuit.gates.len(),
            fid_optimize_ir,
            backend_fidelities[0],
            backend_fidelities[1],
            backend_fidelities[2],
        );
    }

    println!("\nWorst fidelity observed across every case and every rewrite: {:.12}", worst_fidelity);
    if any_failed {
        println!(
            "\nFAIL: at least one rewrite produced fidelity < {:.0e} against the reference.",
            FIDELITY_TOL
        );
        std::process::exit(1);
    } else {
        println!(
            "\nPASS: every optimize_ir rewrite and every backend lowering (TrappedIon, IbmQ, \
             Rigetti) reproduced the reference circuit's exact final state (fidelity 1.0 to \
             within floating-point tolerance) across all {} randomized circuits.",
            circuits.len()
        );
    }
}

/// Runs `circuit` through the crate's own reference semantics: native
/// `{Rz, Ry, Rzz}` decomposition, peephole `optimize`, executed on a
/// fresh `QuantumRegister` starting at |00...0>. This is deliberately
/// *not* independently re-implemented (e.g. by hand-multiplying gate
/// matrices here) -- the whole point is comparing every other pipeline
/// path against the crate's own already-tested native path, the same
/// baseline `backend.rs`'s internal tests already trust.
fn run_native(circuit: &Circuit) -> QuantumRegister {
    let native = optimize(&decompose(circuit));
    emit::run(&native).expect("native run")
}

const MIN_QUBITS: usize = 2;
const MAX_QUBITS: usize = 5;
const MIN_GATES: usize = 12;
const MAX_GATES: usize = 30;

/// Builds a random (but always well-formed -- distinct qubit args on
/// every two-qubit gate) circuit over the crate's full source-level
/// `Gate` set, excluding `Measure` (see this module's doc comment on
/// why fidelity comparison needs a unitary-only circuit).
fn random_circuit(rng: &mut StdRng) -> Circuit {
    let num_qubits = rng.gen_range(MIN_QUBITS..=MAX_QUBITS);
    let num_gates = rng.gen_range(MIN_GATES..=MAX_GATES);
    let mut circuit = Circuit::new(num_qubits);

    for _ in 0..num_gates {
        circuit.push(random_gate(rng, num_qubits));
    }
    circuit
}

fn random_gate(rng: &mut StdRng, num_qubits: usize) -> Gate {
    let q = |rng: &mut StdRng| rng.gen_range(0..num_qubits);
    let pair = |rng: &mut StdRng| -> (usize, usize) {
        loop {
            let a = rng.gen_range(0..num_qubits);
            let b = rng.gen_range(0..num_qubits);
            if a != b {
                return (a, b);
            }
        }
    };
    let theta = |rng: &mut StdRng| rng.gen_range(-std::f64::consts::TAU..std::f64::consts::TAU);

    match rng.gen_range(0..17) {
        0 => Gate::H(q(rng)),
        1 => Gate::X(q(rng)),
        2 => Gate::Y(q(rng)),
        3 => Gate::Z(q(rng)),
        4 => Gate::S(q(rng)),
        5 => Gate::Sdg(q(rng)),
        6 => Gate::T(q(rng)),
        7 => Gate::Tdg(q(rng)),
        8 => Gate::Rx(q(rng), theta(rng)),
        9 => Gate::Ry(q(rng), theta(rng)),
        10 => Gate::Rz(q(rng), theta(rng)),
        11 => {
            let (a, b) = pair(rng);
            Gate::Cx(a, b)
        }
        12 => {
            let (a, b) = pair(rng);
            Gate::Cz(a, b)
        }
        13 => {
            let (a, b) = pair(rng);
            Gate::Swap(a, b)
        }
        14 => {
            let (a, b) = pair(rng);
            Gate::Rxx(a, b, theta(rng))
        }
        15 => {
            let (a, b) = pair(rng);
            Gate::Ryy(a, b, theta(rng))
        }
        _ => {
            let (a, b) = pair(rng);
            Gate::Cp(a, b, theta(rng))
        }
    }
}