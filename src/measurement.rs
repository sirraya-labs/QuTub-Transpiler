//! Statistical correctness test for `Gate::Measure`, per the P0.1
//! roadmap item's definition of done: `QuantumRegister::fidelity`
//! doesn't apply to a measured (collapsed) state, so this can't be a
//! variant of `tests/decompositions.rs`'s direct-fidelity-comparison
//! methodology -- it needs its own, genuinely new one: run many shots,
//! and compare the empirical outcome distribution against the Born-rule
//! probabilities computable from the pre-measurement state, within a
//! stated tolerance.
//!
//! No seeded-RNG entry point is threaded through `emit::run_with_measurement`
//! today, so this uses a large shot count and a wide (many-standard-error)
//! tolerance rather than a fixed seed -- the test is still deterministic
//! in the sense that a real bug (wrong Born-rule wiring, a stuck qubit
//! index, a collapse that doesn't renormalize) would fail it every time,
//! while statistical noise alone essentially never will.

use sirraya_qutub_transpiler::emit;
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::native::decompose;
use sirraya_qutub_transpiler::optimize::optimize;

const SHOTS: usize = 4000;

/// `p_hat` is the empirical frequency of `SHOTS` independent Bernoulli
/// trials with true probability `p_true`; asserts `p_hat` is within 6
/// standard errors of `p_true` (a >99.9999% confidence bound -- this
/// test would need to be extraordinarily unlucky to fail on noise
/// alone, so a failure means something is actually wrong).
fn assert_within_statistical_tolerance(p_hat: f64, p_true: f64, shots: usize, context: &str) {
    let std_err = (p_true * (1.0 - p_true) / shots as f64).sqrt();
    // Floor the tolerance so a near-deterministic outcome (p_true close
    // to 0 or 1, std_err near 0) still allows for the handful of
    // opposite-outcome shots real sampling noise can produce.
    let tolerance = (6.0 * std_err).max(0.01);
    assert!(
        (p_hat - p_true).abs() < tolerance,
        "{}: empirical frequency {:.4} vs ideal {:.4} (tolerance {:.4}, {} shots)",
        context,
        p_hat,
        p_true,
        tolerance,
        shots
    );
}

/// Builds `circuit`, decomposes/optimizes it as usual, and returns the
/// ideal (non-destructive) `(P(0), P(1))` measurement probabilities for
/// `qubit` -- i.e. the same circuit with its `Measure` gates simply not
/// executed, read via `QuantumRegister::get_measurement_probability`
/// instead of collapsing anything.
fn ideal_measurement_probability(circuit: &Circuit, qubit: usize) -> (f64, f64) {
    let native = optimize(&decompose(circuit));
    let reg = emit::run(&native).expect("ideal (measurement-free) circuit must run cleanly");
    reg.get_measurement_probability(qubit)
        .expect("qubit index must be in range")
}

/// Runs `circuit` (which does contain `Measure`) `SHOTS` times via the
/// real `emit::run_with_measurement` path and returns the empirical
/// frequency with which classical bit `clbit` came out `1`.
fn empirical_frequency_of_one(circuit: &Circuit, clbit: usize) -> f64 {
    let native = optimize(&decompose(circuit));
    let mut ones = 0usize;
    for _ in 0..SHOTS {
        let (_, clbits) = emit::run_with_measurement(&native)
            .expect("measuring circuit must run cleanly");
        if clbits[clbit] == 1 {
            ones += 1;
        }
    }
    ones as f64 / SHOTS as f64
}

#[test]
fn measures_a_fair_superposition_close_to_50_50() {
    // H(0) then Measure(0, 0): the textbook case, P(0) = P(1) = 0.5.
    let mut with_measure = Circuit::new(1);
    with_measure.num_clbits = 1;
    with_measure.push(Gate::H(0)).push(Gate::Measure(0, 0));

    let mut without_measure = Circuit::new(1);
    without_measure.push(Gate::H(0));

    let (_, ideal_p1) = ideal_measurement_probability(&without_measure, 0);
    let empirical_p1 = empirical_frequency_of_one(&with_measure, 0);
    assert_within_statistical_tolerance(empirical_p1, ideal_p1, SHOTS, "H then Measure");
    assert!((ideal_p1 - 0.5).abs() < 1e-9, "sanity check: H should give exactly 0.5");
}

#[test]
fn measures_a_biased_rotation_at_the_right_skew() {
    // Ry(theta) skews P(1) away from 0.5 in a known, computable way;
    // this exercises a non-trivial probability, not just the symmetric
    // 50/50 case above.
    let theta = 0.9_f64;
    let mut with_measure = Circuit::new(1);
    with_measure.num_clbits = 1;
    with_measure.push(Gate::Ry(0, theta)).push(Gate::Measure(0, 0));

    let mut without_measure = Circuit::new(1);
    without_measure.push(Gate::Ry(0, theta));

    let (_, ideal_p1) = ideal_measurement_probability(&without_measure, 0);
    let empirical_p1 = empirical_frequency_of_one(&with_measure, 0);
    assert_within_statistical_tolerance(empirical_p1, ideal_p1, SHOTS, "Ry(0.9) then Measure");
    // Ry(theta)|0> = cos(theta/2)|0> + sin(theta/2)|1>, so P(1) = sin^2(theta/2).
    let expected_p1 = (theta / 2.0).sin().powi(2);
    assert!((ideal_p1 - expected_p1).abs() < 1e-9);
}

#[test]
fn measures_the_correct_qubit_out_of_an_entangled_pair() {
    // Bell pair: H(0), Cx(0,1), Measure(1, 0). Marginal on qubit 1 alone
    // is still 50/50 even though the *joint* state is entangled -- this
    // pins down that Measure reads the qubit it's actually given, not
    // qubit 0 by accident.
    let mut with_measure = Circuit::new(2);
    with_measure.num_clbits = 1;
    with_measure
        .push(Gate::H(0))
        .push(Gate::Cx(0, 1))
        .push(Gate::Measure(1, 0));

    let mut without_measure = Circuit::new(2);
    without_measure.push(Gate::H(0)).push(Gate::Cx(0, 1));

    let (_, ideal_p1) = ideal_measurement_probability(&without_measure, 1);
    let empirical_p1 = empirical_frequency_of_one(&with_measure, 0);
    assert_within_statistical_tolerance(empirical_p1, ideal_p1, SHOTS, "Bell pair, measure qubit 1");
    assert!((ideal_p1 - 0.5).abs() < 1e-9);
}
