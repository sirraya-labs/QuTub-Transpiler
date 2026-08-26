//! Fault-tolerant resource estimation: T-count and T-depth, the
//! currency fault-tolerant (surface-code-era) quantum computing is
//! actually budgeted in -- as distinct from [`crate::fidelity`]'s
//! NISQ-era depolarizing-error budget, which this module deliberately
//! does not replace or subsume.
//!
//! # Why T-count, not gate count
//! Under most quantum error correction codes (surface codes included),
//! Clifford gates (`H`, `S`, `Sdg`, `X`, `Y`, `Z`, `Cx`, `Cz`, `Swap`)
//! are comparatively cheap -- they can be implemented transversally or
//! by Pauli-frame bookkeeping, without consuming a distilled resource
//! state. A `T` gate cannot: it needs a magic state, and magic-state
//! distillation is, in most current architectures, the dominant cost
//! of the entire computation -- often cited as consuming the large
//! majority of a fault-tolerant algorithm's physical qubits and time.
//! So the question "how many T gates does this circuit need" is a far
//! more decision-relevant number, this early in a circuit's life, than
//! "how many gates does this circuit have."
//!
//! # What this module does and does not estimate
//! This estimates **logical** T-count/T-depth: how many T gates (or
//! T-equivalent rotation-synthesis budget) the circuit needs, and how
//! many of those can run in parallel. It deliberately does **not**
//! estimate physical qubit count, code distance, or wall-clock time --
//! those depend on a specific error-correcting code, its distance, the
//! target logical error rate, and the physical error rate of a
//! specific device, none of which this module has an opinion on. That
//! is real, separate, and necessarily hardware-specific follow-on
//! work; conflating it with this module's purely algorithmic count
//! would make this module's numbers only as trustworthy as a set of
//! assumptions about hardware that doesn't exist yet.
//!
//! # How a continuous rotation gets a T-count
//! This crate's native gate set is `{Rz, Ry, Rzz}` -- continuous-angle
//! rotations, not the discrete Clifford+T set fault-tolerant algorithms
//! are actually compiled to. An arbitrary single-qubit rotation has no
//! *exact* finite Clifford+T decomposition in general; it can only be
//! *approximated* to within a target precision `epsilon`, at a T-gate
//! cost that grows as that precision tightens. This module uses the
//! well-known asymptotic result for optimal ancilla-free Clifford+T
//! synthesis of a single-qubit z-rotation (Ross & Selinger, "Optimal
//! ancilla-free Clifford+T approximation of z-rotations", 2016):
//! roughly `3 * log2(1/epsilon)` T gates for an epsilon-accurate
//! approximation. This is the *asymptotically optimal* count a real
//! synthesis algorithm (e.g. their own `gridsynth`) can achieve; it is
//! not what a naive Solovay-Kitaev synthesis would cost (which is
//! asymptotically worse), and it is an estimate, not an actual
//! synthesis -- this module never emits real Clifford+T gates, only a
//! number. Getting an exact count for a specific circuit means running
//! a real synthesis tool against the real target angles.
//!
//! A rotation whose angle is an exact multiple of `pi/2` is already
//! Clifford (0 T gates); an exact odd multiple of `pi/4` is exactly
//! one `T`/`Tdg` up to a free Clifford correction. Both are detected
//! and costed exactly, not run through the asymptotic estimate --
//! `Rz(pi/2)` (i.e. `S`) should cost 0 T gates, not `3*log2(1/epsilon)`
//! of them, and this module gets that right rather than pessimistic.
//!
//! # Two-qubit and multi-rotation gates
//! `Rzz(a, b, theta)` is `Cx(a,b) . Rz(b, theta) . Cx(a,b)` (see
//! `native.rs`'s own decomposition) -- the two `Cx`s are Clifford, so
//! `Rzz`'s T-cost is exactly one single-qubit rotation's T-cost at the
//! same angle. `Rxx`/`Ryy` reduce to `Rzz` by Clifford conjugation
//! (also in `native.rs`), so the same reasoning applies transitively.
//! Rather than re-deriving any of this, this module runs the circuit
//! through the crate's own [`crate::native::decompose`] +
//! [`crate::optimize::optimize`] first, then costs only the resulting
//! `Rz`/`Ry`/`Rzz` -- so the T-count is grounded in the exact same
//! identities the rest of the crate already tests against the real
//! simulator, not a second, independently-derived model of what those
//! gates cost.
//!
//! # Classical control
//! A [`crate::ir::Gate::If`]-conditioned gate costs exactly what its
//! `inner` would unconditioned -- the classical condition itself is
//! assumed free (a fast enough decoder to keep up with the code cycle;
//! see this crate's own architecture notes on why that assumption is a
//! real, separate hardware constraint this module doesn't model). A
//! `Measure` costs 0 T gates but is tracked as its own count, since
//! measurement is a real resource (and, on some codes, the *dominant*
//! one for syndrome extraction) that a T-count alone would hide.

use crate::ir::{Circuit, Gate};
use crate::native::{decompose, NativeGate};
use crate::optimize::optimize;
use std::f64::consts::{PI, TAU};

/// Default precision target for an approximated (non-exact) rotation,
/// if the caller doesn't have a more specific one in mind. `1e-10` is
/// a common rule-of-thumb starting point for a single rotation inside
/// a larger algorithm (tighter than the end-to-end algorithmic
/// accuracy target, since errors from many rotations can accumulate) --
/// not a universal constant, and callers with a real accuracy budget
/// should pass their own via [`estimate_circuit_resources_with_epsilon`].
pub const DEFAULT_ROTATION_EPSILON: f64 = 1e-10;

/// How one rotation's T-cost was determined -- kept alongside the
/// count itself so a caller can tell an exact 0 (a Clifford angle)
/// apart from an estimate that happens to be small, or flag how many
/// rotations in a circuit are exact vs. approximated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationSynthesis {
    /// Angle is an exact multiple of `2*pi` -- the identity; 0 gates
    /// of any kind, not even a Clifford one.
    Identity,
    /// Angle is an exact multiple of `pi/2` -- already Clifford
    /// (`Z`/`S`/`Sdg`, or the identity's own multiples); 0 T gates.
    Clifford,
    /// Angle is an exact odd multiple of `pi/4` -- exactly one
    /// `T`/`Tdg` up to a free Clifford correction.
    ExactT,
    /// No exact finite Clifford+T decomposition; costed via the
    /// Ross-Selinger asymptotic estimate at the epsilon this
    /// estimate was run with -- see this module's doc comment.
    Approximate,
}

/// One rotation's contribution to a circuit's resource budget.
#[derive(Debug, Clone, Copy)]
pub struct RotationCost {
    pub synthesis: RotationSynthesis,
    pub t_count: usize,
}

/// The full resource budget for a circuit: T-count/T-depth (the two
/// numbers that matter for a fault-tolerant cost estimate -- see this
/// module's doc comment), plus enough supporting detail to sanity-check
/// where they came from.
#[derive(Debug, Clone, Default)]
pub struct ResourceBudget {
    /// Total T gates (exact `T`/`Tdg` gates in the source, plus every
    /// rotation's synthesis cost at whatever `epsilon` this estimate
    /// was run with).
    pub t_count: usize,
    /// Longest chain of T-consuming operations on any single qubit --
    /// a lower bound on how many sequential magic-state consumption
    /// rounds the circuit needs, the same "critical path" sense
    /// `depth` is used in elsewhere in this crate's examples, just
    /// counting only T-contributing layers rather than every gate.
    ///
    /// **This is a conservative upper bound, not an optimized T-depth.**
    /// It counts a new layer every time a T-consuming gate follows
    /// another one on the same wire in program order. A real T-depth
    /// optimization pass can often do better, because Clifford gates
    /// between two T gates on the same wire don't have to physically
    /// block them the way this simple model assumes -- they can often
    /// be tracked in software (Pauli-frame bookkeeping) and commuted
    /// out of the way instead. Getting the *optimized* number needs a
    /// real T-depth-reduction pass (a separate, real piece of
    /// follow-on work), not just this circuit-order count.
    pub t_depth: usize,
    /// Every Clifford gate the circuit needs (`H`/`S`/`Sdg`/`X`/`Y`/
    /// `Z`/`Cx`/`Cz`/`Swap`, plus the free Clifford part of every
    /// `RotationSynthesis::Clifford`/`ExactT` rotation) -- reported
    /// because "how many Cliffords" is a real, separate cost on
    /// several real error-correction schemes (lattice surgery merges,
    /// for instance), even though it's not the dominant one T-count is.
    pub clifford_count: usize,
    /// Real measurements in the circuit -- tracked separately from
    /// T-count for the reason given in this module's doc comment
    /// (measurement is a real, sometimes-dominant resource of its own,
    /// not something a T-count captures).
    pub measurement_count: usize,
    /// How many rotations were costed exactly (`Identity`/`Clifford`/
    /// `ExactT`) vs. via the asymptotic estimate (`Approximate`) --
    /// lets a caller see how much of `t_count` is exact bookkeeping
    /// vs. an estimate that would change under a different `epsilon`.
    pub exact_rotations: usize,
    pub approximated_rotations: usize,
}

/// Estimates `circuit`'s fault-tolerant resource budget at
/// [`DEFAULT_ROTATION_EPSILON`] -- see
/// [`estimate_circuit_resources_with_epsilon`] to choose a different
/// precision target.
pub fn estimate_circuit_resources(circuit: &Circuit) -> ResourceBudget {
    estimate_circuit_resources_with_epsilon(circuit, DEFAULT_ROTATION_EPSILON)
}

/// As [`estimate_circuit_resources`], but at caller-chosen rotation
/// precision `epsilon`. Runs `circuit` through this crate's own
/// [`decompose`] + [`optimize`] first (see this module's doc comment
/// on why), so the reported T-count reflects the circuit *after* the
/// same peephole cleanup `native.rs`'s pipeline already applies before
/// execution -- not the raw, unoptimized source gate count, which
/// would overstate the cost of a circuit with cancelling rotations.
pub fn estimate_circuit_resources_with_epsilon(circuit: &Circuit, epsilon: f64) -> ResourceBudget {
    let native = optimize(&decompose(circuit));
    let mut budget = ResourceBudget::default();
    let mut last_t_layer = vec![0usize; native.num_qubits];
    for gate in &native.gates {
        cost_native_gate(gate, epsilon, &mut budget, &mut last_t_layer);
    }
    budget.t_depth = last_t_layer.into_iter().max().unwrap_or(0);
    budget
}

fn cost_native_gate(
    gate: &NativeGate,
    epsilon: f64,
    budget: &mut ResourceBudget,
    last_t_layer: &mut [usize],
) {
    match gate {
        NativeGate::Rz(q, angle) | NativeGate::Ry(q, angle) => {
            apply_rotation_cost(*q, *angle, epsilon, budget, last_t_layer);
        }
        NativeGate::Rzz(a, b, angle) => {
            // Cx(a,b) . Rz(b, angle) . Cx(a,b) -- the Cxs are Clifford
            // (counted below), the T-cost lives entirely in the middle
            // Rz, and it's a real event on *both* wires for T-depth
            // purposes (the CNOTs entangle them around it).
            budget.clifford_count += 2; // the two sandwiching Cx gates
            let cost = rotation_t_cost(*angle, epsilon);
            record_rotation(cost, budget);
            if cost.t_count > 0 {
                let layer = last_t_layer[*a].max(last_t_layer[*b]) + 1;
                last_t_layer[*a] = layer;
                last_t_layer[*b] = layer;
            }
        }
        NativeGate::Measure(..) => {
            budget.measurement_count += 1;
        }
        NativeGate::If(_, inner) => {
            // The condition itself is assumed free (see this module's
            // doc comment on the fast-decoder assumption); `inner`
            // costs exactly what it would unconditioned.
            cost_native_gate(inner, epsilon, budget, last_t_layer);
        }
    }
}

fn apply_rotation_cost(
    q: usize,
    angle: f64,
    epsilon: f64,
    budget: &mut ResourceBudget,
    last_t_layer: &mut [usize],
) {
    let cost = rotation_t_cost(angle, epsilon);
    record_rotation(cost, budget);
    if cost.t_count > 0 {
        last_t_layer[q] += 1;
    }
}

fn record_rotation(cost: RotationCost, budget: &mut ResourceBudget) {
    budget.t_count += cost.t_count;
    match cost.synthesis {
        RotationSynthesis::Identity => {
            budget.exact_rotations += 1;
        }
        RotationSynthesis::Clifford => {
            budget.clifford_count += 1;
            budget.exact_rotations += 1;
        }
        RotationSynthesis::ExactT => {
            // The T/Tdg itself is in t_count already; the "up to a
            // free Clifford correction" part of an exact synthesis is
            // the Clifford half of that decomposition.
            budget.clifford_count += 1;
            budget.exact_rotations += 1;
        }
        RotationSynthesis::Approximate => {
            budget.approximated_rotations += 1;
        }
    }
}

/// Tolerance for treating an angle as an *exact* multiple of `pi/4`
/// (and therefore `pi/2`) rather than falling through to the
/// asymptotic estimate. Matches `native.rs`'s own `EPS` -- the same
/// "this many radians of floating-point slop is a rounding error, not
/// a real angle" judgment call the rest of the crate already makes.
const EPS_ANGLE: f64 = 1e-9;

/// Classifies and costs a single rotation angle -- shared by every
/// call site above so the exact-vs-approximate boundary is judged
/// exactly once. See this module's doc comment for the reasoning
/// behind each case.
fn rotation_t_cost(theta: f64, epsilon: f64) -> RotationCost {
    let wrapped = theta.rem_euclid(TAU);
    let near_zero = wrapped < EPS_ANGLE || (TAU - wrapped) < EPS_ANGLE;
    if near_zero {
        return RotationCost { synthesis: RotationSynthesis::Identity, t_count: 0 };
    }

    let quarter_turns = wrapped / (PI / 2.0);
    if (quarter_turns - quarter_turns.round()).abs() < EPS_ANGLE {
        return RotationCost { synthesis: RotationSynthesis::Clifford, t_count: 0 };
    }

    let eighth_turns = wrapped / (PI / 4.0);
    if (eighth_turns - eighth_turns.round()).abs() < EPS_ANGLE {
        // Already excluded exact pi/2 multiples above, so surviving
        // here means this is specifically an *odd* multiple of pi/4.
        return RotationCost { synthesis: RotationSynthesis::ExactT, t_count: 1 };
    }

    // Ross-Selinger asymptotic optimal count: ~3*log2(1/epsilon).
    // `epsilon` must be in (0, 1) for this to mean anything as a
    // precision target; callers passing an invalid epsilon get a
    // saturating fallback rather than a NaN/negative T-count.
    let safe_epsilon = if epsilon > 0.0 && epsilon < 1.0 { epsilon } else { DEFAULT_ROTATION_EPSILON };
    let t_count = (3.0 * (1.0 / safe_epsilon).log2()).ceil().max(1.0) as usize;
    RotationCost { synthesis: RotationSynthesis::Approximate, t_count }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_angle_costs_nothing() {
        let cost = rotation_t_cost(0.0, DEFAULT_ROTATION_EPSILON);
        assert_eq!(cost.synthesis, RotationSynthesis::Identity);
        assert_eq!(cost.t_count, 0);
        // Exact 2*pi should be treated the same as exact 0.
        let cost2 = rotation_t_cost(TAU, DEFAULT_ROTATION_EPSILON);
        assert_eq!(cost2.synthesis, RotationSynthesis::Identity);
    }

    #[test]
    fn pi_over_2_multiples_are_clifford() {
        for k in [1, 2, 3, -1, -2] {
            let cost = rotation_t_cost(k as f64 * PI / 2.0, DEFAULT_ROTATION_EPSILON);
            assert_eq!(
                cost.synthesis,
                RotationSynthesis::Clifford,
                "angle {} * pi/2 should be exact Clifford",
                k
            );
            assert_eq!(cost.t_count, 0);
        }
    }

    #[test]
    fn odd_pi_over_4_multiples_cost_exactly_one_t() {
        for k in [1, 3, 5, 7, -1, -3] {
            let cost = rotation_t_cost(k as f64 * PI / 4.0, DEFAULT_ROTATION_EPSILON);
            assert_eq!(
                cost.synthesis,
                RotationSynthesis::ExactT,
                "angle {} * pi/4 should be exact T/Tdg",
                k
            );
            assert_eq!(cost.t_count, 1);
        }
    }

    #[test]
    fn generic_angle_uses_the_asymptotic_estimate_and_shrinks_with_epsilon() {
        let loose = rotation_t_cost(0.123456, 1e-3);
        let tight = rotation_t_cost(0.123456, 1e-12);
        assert_eq!(loose.synthesis, RotationSynthesis::Approximate);
        assert_eq!(tight.synthesis, RotationSynthesis::Approximate);
        assert!(
            tight.t_count > loose.t_count,
            "a tighter epsilon should never cost fewer T gates: loose={} tight={}",
            loose.t_count,
            tight.t_count
        );
    }

    #[test]
    fn invalid_epsilon_falls_back_rather_than_producing_nonsense() {
        let cost = rotation_t_cost(0.123456, -1.0);
        assert_eq!(cost.synthesis, RotationSynthesis::Approximate);
        assert!(cost.t_count > 0);
    }

    #[test]
    fn h_gate_is_pure_clifford_zero_t_count() {
        let mut c = Circuit::new(1);
        c.push(Gate::H(0));
        let budget = estimate_circuit_resources(&c);
        assert_eq!(budget.t_count, 0);
        assert!(budget.clifford_count > 0);
    }

    #[test]
    fn t_gate_costs_exactly_one_t() {
        let mut c = Circuit::new(1);
        c.push(Gate::T(0));
        let budget = estimate_circuit_resources(&c);
        assert_eq!(budget.t_count, 1);
    }

    #[test]
    fn arbitrary_rotation_costs_more_than_zero_t() {
        let mut c = Circuit::new(1);
        c.push(Gate::Rz(0, 0.123456));
        let budget = estimate_circuit_resources(&c);
        assert!(budget.t_count > 0);
        assert_eq!(budget.approximated_rotations, 1);
    }

    #[test]
    fn cx_is_free_two_qubit_rzz_costs_one_rotations_worth() {
        let mut c = Circuit::new(2);
        c.push(Gate::Cx(0, 1));
        let cx_budget = estimate_circuit_resources(&c);
        assert_eq!(cx_budget.t_count, 0);

        let mut c2 = Circuit::new(2);
        c2.push(Gate::Rzz(0, 1, 0.123456));
        let rzz_budget = estimate_circuit_resources(&c2);
        let single_rotation = rotation_t_cost(0.123456, DEFAULT_ROTATION_EPSILON).t_count;
        assert_eq!(rzz_budget.t_count, single_rotation);
    }

    #[test]
    fn measure_costs_zero_t_but_is_tracked_separately() {
        let mut c = Circuit::new(1);
        c.num_clbits = 1;
        c.push(Gate::Measure(0, 0));
        let budget = estimate_circuit_resources(&c);
        assert_eq!(budget.t_count, 0);
        assert_eq!(budget.measurement_count, 1);
    }

    #[test]
    fn conditioned_gate_costs_the_same_as_unconditioned() {
        let mut plain = Circuit::new(1);
        plain.push(Gate::Rz(0, 0.123456));
        let plain_budget = estimate_circuit_resources(&plain);

        let mut conditioned = Circuit::new(2);
        conditioned.num_clbits = 1;
        conditioned.push(Gate::If(vec![(0, true)], Box::new(Gate::Rz(1, 0.123456))));
        let conditioned_budget = estimate_circuit_resources(&conditioned);

        assert_eq!(plain_budget.t_count, conditioned_budget.t_count);
    }

    #[test]
    fn t_depth_counts_sequential_t_layers_not_total_t_count() {
        // Two T gates on the SAME qubit, in sequence, with an
        // intervening gate on a DIFFERENT qubit so merge_pass can't
        // fuse them (adjacent-same-qubit T . T = S is a real,
        // physically correct fusion this estimate correctly benefits
        // from -- see the note below -- so this test has to actually
        // prevent that fusion to test what it means to test).
        let mut sequential = Circuit::new(2);
        sequential.push(Gate::T(0)).push(Gate::X(1)).push(Gate::T(0));
        let seq_budget = estimate_circuit_resources(&sequential);
        assert_eq!(seq_budget.t_count, 2);
        assert_eq!(seq_budget.t_depth, 2);

        // Two T gates on DIFFERENT qubits -> t_depth 1 (parallel).
        let mut parallel = Circuit::new(2);
        parallel.push(Gate::T(0)).push(Gate::T(1));
        let par_budget = estimate_circuit_resources(&parallel);
        assert_eq!(par_budget.t_count, 2);
        assert_eq!(par_budget.t_depth, 1);
    }

    #[test]
    fn adjacent_same_qubit_t_gates_genuinely_fuse_to_a_clifford() {
        // T . T == S exactly (both are Rz(pi/4) up to phase, and
        // Rz(pi/4) . Rz(pi/4) == Rz(pi/2) == S). optimize()'s own
        // merge_pass performs exactly this fusion since it merges any
        // two adjacent same-qubit Rz's regardless of the resulting
        // angle -- and estimate_circuit_resources runs that same
        // optimize() before costing anything (see this module's own
        // doc comment on why), so it should report this correctly as
        // 0 T gates, not 2. A naive per-source-gate estimator that
        // summed each T's cost independently would get this wrong.
        let mut c = Circuit::new(1);
        c.push(Gate::T(0)).push(Gate::T(0));
        let budget = estimate_circuit_resources(&c);
        assert_eq!(budget.t_count, 0, "adjacent T.T should fuse to S (Clifford) before costing");
    }

    #[test]
    fn tighter_epsilon_never_decreases_t_count_for_a_whole_circuit() {
        let mut c = Circuit::new(1);
        c.push(Gate::Rz(0, 0.7)).push(Gate::Ry(0, 1.9));
        let loose = estimate_circuit_resources_with_epsilon(&c, 1e-2);
        let tight = estimate_circuit_resources_with_epsilon(&c, 1e-15);
        assert!(tight.t_count >= loose.t_count);
    }
}