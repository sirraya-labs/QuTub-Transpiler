#![no_main]

//! Structural robustness target for the transformation pipeline
//! (`optimize_ir` -> `decompose` -> `optimize` -> `lower` per backend).
//!
//! `examples/verify_equivalence.rs` already proves *semantic*
//! preservation (fidelity vs. simulator) over 40 fixed-seed circuits —
//! that stays the authoritative correctness oracle and this target does
//! not duplicate it. What a 40-circuit fixed corpus structurally cannot
//! do is explore the combinatorial edge cases a coverage-guided fuzzer
//! finds: empty circuits, single-qubit registers, back-to-back
//! `Measure`s on the same clbit, deeply nested `If` conditions,
//! pathological SWAP/Rzz interleavings that stress `ir_optimize.rs`'s
//! commuting pass or `route.rs`'s SWAP insertion. This target's only
//! claim is "the pipeline does not panic or hang" — a crash here is a
//! real bug regardless of what the output circuit turns out to be.
//!
//! Any crash this finds should be minimized (`cargo fuzz tmin`), then:
//!  1. turned into a #[test] under tests/ that replays the exact
//!     minimized circuit through the same call sequence, and
//!  2. if fidelity against the simulator is also wrong (not just a
//!     panic), added to verify_equivalence.rs's fixed corpus too, so
//!     the *semantic* oracle also has it forever.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::{decompose, lower, optimize, optimize_ir, Backend};

const MAX_QUBITS: usize = 6;
const MAX_GATES: usize = 64;

/// Byte-driven gate choice — mirrors the shape of
/// `verify_equivalence.rs`'s `random_circuit`, but pulls choices from
/// libFuzzer's coverage-guided `Unstructured` bytes instead of a fixed
/// RNG seed, so the corpus can evolve toward whatever combination
/// actually finds new coverage.
#[derive(Arbitrary, Debug)]
struct FuzzCircuit {
    num_qubits_seed: u8,
    num_clbits_seed: u8,
    ops: Vec<FuzzOp>,
}

#[derive(Arbitrary, Debug)]
enum FuzzOp {
    H(u8),
    X(u8),
    Y(u8),
    Z(u8),
    S(u8),
    T(u8),
    Rx(u8, f64),
    Ry(u8, f64),
    Rz(u8, f64),
    Cx(u8, u8),
    Cz(u8, u8),
    Swap(u8, u8),
    Rzz(u8, u8, f64),
    Cp(u8, u8, f64),
    Measure(u8, u8),
}

fn q(n: usize, v: u8) -> usize {
    if n == 0 { 0 } else { (v as usize) % n }
}

fn clamp_theta(t: f64) -> f64 {
    if t.is_finite() {
        t % std::f64::consts::TAU
    } else {
        0.0
    }
}

fuzz_target!(|fc: FuzzCircuit| {
    let num_qubits = 1 + (fc.num_qubits_seed as usize % MAX_QUBITS);
    let num_clbits = 1 + (fc.num_clbits_seed as usize % MAX_QUBITS);

    let mut circuit = Circuit::new(num_qubits);
    circuit.num_clbits = num_clbits;

    for op in fc.ops.into_iter().take(MAX_GATES) {
        let g = match op {
            FuzzOp::H(a) => Gate::H(q(num_qubits, a)),
            FuzzOp::X(a) => Gate::X(q(num_qubits, a)),
            FuzzOp::Y(a) => Gate::Y(q(num_qubits, a)),
            FuzzOp::Z(a) => Gate::Z(q(num_qubits, a)),
            FuzzOp::S(a) => Gate::S(q(num_qubits, a)),
            FuzzOp::T(a) => Gate::T(q(num_qubits, a)),
            FuzzOp::Rx(a, t) => Gate::Rx(q(num_qubits, a), clamp_theta(t)),
            FuzzOp::Ry(a, t) => Gate::Ry(q(num_qubits, a), clamp_theta(t)),
            FuzzOp::Rz(a, t) => Gate::Rz(q(num_qubits, a), clamp_theta(t)),
            FuzzOp::Cx(a, b) => Gate::Cx(q(num_qubits, a), q(num_qubits, b)),
            FuzzOp::Cz(a, b) => Gate::Cz(q(num_qubits, a), q(num_qubits, b)),
            FuzzOp::Swap(a, b) => Gate::Swap(q(num_qubits, a), q(num_qubits, b)),
            FuzzOp::Rzz(a, b, t) => Gate::Rzz(q(num_qubits, a), q(num_qubits, b), clamp_theta(t)),
            FuzzOp::Cp(a, b, t) => Gate::Cp(q(num_qubits, a), q(num_qubits, b), clamp_theta(t)),
            FuzzOp::Measure(a, c) => Gate::Measure(q(num_qubits, a), q(num_clbits, c)),
        };
        // Reject same-qubit two-qubit gates rather than let the pipeline
        // decide what that means — that's a QASM-level validity rule,
        // not a pipeline robustness question this target is aimed at.
        if let Gate::Cx(a, b) | Gate::Cz(a, b) | Gate::Swap(a, b)
        | Gate::Rzz(a, b, _) | Gate::Cp(a, b, _) = g
        {
            if a == b {
                continue;
            }
        }
        circuit.gates.push(g);
    }

    // Exercise both real pipeline paths the roadmap document calls out:
    // Source -> QuTub IR -> [source-level optimize] -> native decompose
    // -> [native optimize] (fuzzed for panics on its own: this is the
    // path `run_native`/`decompositions.rs` simulate against), and
    // separately Source -> optimize_ir -> backend lower (`lower` takes
    // the source-level Circuit directly and does its own decompose+
    // route internally — it is not fed by the native/native_opt path;
    // see examples/verify_equivalence.rs).
    let optimized = optimize_ir(&circuit);
    let native = decompose(&optimized);
    let _native_opt = optimize(&native);

    for backend in [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti] {
        let _ = lower(&optimized, backend);
    }
});
