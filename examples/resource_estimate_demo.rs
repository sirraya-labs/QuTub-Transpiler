//! Fault-tolerant resource estimation: for a handful of circuits,
//! prints the T-count / T-depth budget [`resource_estimate`] computes
//! -- the number a fault-tolerant target is actually designed and
//! rejected against, the same role [`fidelity_scaling`]'s NISQ-era
//! depolarizing-error estimate plays for a near-term backend. See
//! `resource_estimate.rs`'s own module doc for what this does and
//! deliberately does not estimate (no physical qubit count, no code
//! distance -- those need a real error-correcting code and a real
//! target logical error rate, which this module has no opinion on).
//!
//! Run with: `cargo run --example resource_estimate_demo`

use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::resource_estimate::{
    estimate_circuit_resources, estimate_circuit_resources_with_epsilon, ResourceBudget,
};

/// H on qubit 0, then a CNOT ladder out to every other qubit -- the
/// standard GHZ-state preparation circuit. Pure Clifford: every gate
/// here (H, Cx) should cost exactly 0 T gates.
fn ghz_circuit(num_qubits: usize) -> Circuit {
    let mut c = Circuit::new(num_qubits);
    c.push(Gate::H(0));
    for q in 1..num_qubits {
        c.push(Gate::Cx(0, q));
    }
    c
}

/// n-qubit Quantum Fourier Transform: H plus controlled-phase rotations
/// with rapidly shrinking angles (`pi / 2^k`), then a reversal via
/// Swaps. Interesting for this demo specifically because it's a mix:
/// some of those angles land on exact Clifford+T points, most don't --
/// so the reported budget is a genuine mix of `ExactT` and
/// `Approximate` rotations, not one or the other.
fn qft(n: usize) -> Circuit {
    let mut c = Circuit::new(n);
    for i in 0..n {
        c.push(Gate::H(i));
        for j in (i + 1)..n {
            let lambda = std::f64::consts::PI / (1u32 << (j - i)) as f64;
            c.push(Gate::Cp(j, i, lambda));
        }
    }
    for i in 0..(n / 2) {
        c.push(Gate::Swap(i, n - 1 - i));
    }
    c
}

fn print_budget(label: &str, budget: &ResourceBudget) {
    println!(
        "{:<28} T-count={:>6}  T-depth={:>5}  Clifford={:>6}  Measurements={:>3}  \
         (exact rotations={}, approximated={})",
        label,
        budget.t_count,
        budget.t_depth,
        budget.clifford_count,
        budget.measurement_count,
        budget.exact_rotations,
        budget.approximated_rotations,
    );
}

fn main() {
    println!("=== Pure Clifford circuits: T-count should be exactly 0 ===\n");
    for n in [2usize, 8, 32] {
        let budget = estimate_circuit_resources(&ghz_circuit(n));
        print_budget(&format!("ghz_{n}"), &budget);
        assert_eq!(budget.t_count, 0, "a pure-Clifford GHZ circuit must cost 0 T gates");
    }

    println!("\n=== One rotation at a time, showing each synthesis case ===\n");
    let cases: [(&str, f64); 5] = [
        ("Rz(0)              -- Identity", 0.0),
        ("Rz(pi/2)           -- Clifford (S)", std::f64::consts::FRAC_PI_2),
        ("Rz(pi/4)           -- ExactT (T)", std::f64::consts::FRAC_PI_4),
        ("Rz(3*pi/4)         -- ExactT (Tdg up to Clifford)", 3.0 * std::f64::consts::FRAC_PI_4),
        ("Rz(0.37)           -- Approximate (generic angle)", 0.37),
    ];
    for (label, angle) in cases {
        let mut c = Circuit::new(1);
        c.push(Gate::Rz(0, angle));
        let budget = estimate_circuit_resources(&c);
        print_budget(label, &budget);
    }

    println!("\n=== QFT: a realistic mix of exact and approximated rotations ===\n");
    for n in [3usize, 5, 8] {
        let budget = estimate_circuit_resources(&qft(n));
        print_budget(&format!("qft_{n}"), &budget);
    }

    println!(
        "\n(QFT's controlled-phase angles are pi/2^k for k = 1, 2, 3, ... -- k=1 (pi/2) and\n\
         k=2 (pi/4) land on exact Clifford+T points once decomposed; every k >= 3 does not,\n\
         so QFT's T-count grows both from more gates *and* a growing fraction of them needing\n\
         the asymptotic estimate rather than an exact count, as n increases.)"
    );

    println!("\n=== Tightening the rotation precision epsilon raises T-count ===\n");
    let mut single_rotation = Circuit::new(1);
    single_rotation.push(Gate::Rz(0, 0.37));
    for epsilon in [1e-3, 1e-6, 1e-10, 1e-15] {
        let budget = estimate_circuit_resources_with_epsilon(&single_rotation, epsilon);
        println!(
            "  epsilon={:<8.0e}  T-count={:>3}  (Ross-Selinger: ~3*log2(1/epsilon))",
            epsilon, budget.t_count
        );
    }

    println!(
        "\nThis is the fundamental fault-tolerant accuracy/cost trade-off: every digit of\n\
         precision on a single arbitrary rotation costs real, additional T gates -- exactly\n\
         why real fault-tolerant algorithm design tries hard to land on exact Clifford+T\n\
         angles (multiples of pi/4) wherever the algorithm allows it, rather than leaving\n\
         rotations at whatever angle falls out of a classical sub-routine unexamined."
    );

    // Sanity check consistent with resource_estimate.rs's own test
    // suite: a tighter epsilon should never *reduce* the reported cost.
    let loose = estimate_circuit_resources_with_epsilon(&single_rotation, 1e-3);
    let tight = estimate_circuit_resources_with_epsilon(&single_rotation, 1e-15);
    assert!(tight.t_count >= loose.t_count);
}
