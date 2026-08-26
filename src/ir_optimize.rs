//! Source-level (pre-decomposition) optimization passes over
//! [`Circuit`], distinct from [`crate::optimize`]'s peephole pass over
//! already-decomposed native gates. Two kinds of gate pair cancel here:
//! literal self-inverses (`H;H`, `Cx(a,b);Cx(a,b)`, `Swap(a,b);Swap(a,b)`,
//! ...) and explicit inverse pairs (`S;Sdg`, `T;Tdg`). Both only fire on
//! *adjacent* occurrences with identical qubit arguments, so the useful
//! part of this module is the commuting-reorder pass that slides gates
//! past each other to make non-adjacent cancellable pairs adjacent.
//!
//! The base commutation rule is the universally true one: **two gates
//! with disjoint qubit sets commute**, unconditionally, because they
//! act on different tensor factors. On top of that, three gate-specific
//! rules are implemented:
//!
//! 1. **Any diagonal single-qubit gate (`Z`, `S`, `Sdg`, `T`, `Tdg`, `Rz`)
//!    commutes through `Cx(control, target)` when `q == control`, but not
//!    when `q == target`** -- `Cx` is diagonal in its control's computational
//!    basis (`|0><0| (x) I + |1><1| (x) X`), and diagonal single-qubit gates
//!    are diagonal in the same basis, so they act as commuting operators;
//!    the target wire carries the actual `X`, which does not commute with
//!    target-side diagonal phase gates. This generalizes the original `Rz`-only
//!    rule to all diagonal single-qubit gates.
//!
//! 2. **Any diagonal single-qubit gate (`Z`, `S`, `Sdg`, `T`, `Tdg`,
//!    `Rz`) on qubit `q` commutes through `Cz(a, b)` when `q == a` or
//!    `q == b`** -- unlike `Cx`, `Cz` is fully diagonal in the
//!    computational basis on *both* of its wires
//!    (`diag(1, 1, 1, -1)`), so there is no asymmetric "control vs.
//!    target" distinction to preserve: a diagonal gate on *either* wire
//!    is simultaneously diagonalizable with `Cz` and commutes through
//!    it unconditionally. This mirrors `backend.rs`'s
//!    `optimize_cancels_cz_pair_around_rz_on_either_wire` test at the
//!    native level, brought up to this source level the same way rule 1
//!    was. A non-diagonal single-qubit gate (`H`, `X`, `Y`, `Rx`, `Ry`)
//!    does *not* get this treatment -- e.g. `X` anticommutes with the
//!    `Z`-type action `Cz` applies conditionally, so this rule only ever
//!    fires for the six diagonal gates named above.
//!
//! 3. **Any X-basis single-qubit gate (`X`, `Rx`) commutes through
//!    `Cx(control, target)` when `q == target`, but not when `q == control`**
//!    -- the mirror image of rule 1. `Cx` XOR's the control into the target
//!    (`|c, t> -> |c, t XOR c>`), and `X`/`Rx` generate operations in the
//!    X-basis; XOR is commutative and associative, so a target-side X-basis
//!    gate before or after the `Cx` commutes -- confirmed by direct matrix
//!    multiplication. A control-side X-basis gate, by contrast, changes
//!    *which* target output the `Cx` selects.
//!
//! No other gate-type-specific commutation rule is implemented -- that
//! class of rule is real and used by production transpilers, but each
//! one needs its own derivation and test the same way these three and
//! the two-qubit identities in `native.rs` did, and getting one wrong
//! silently produces a wrong circuit rather than a missed optimization.
//! Disjoint-support commutativity needs no such proof.
//!
//! The `tests` submodule below checks every pass end to end against the
//! real simulator (same methodology as `tests/decompositions.rs`): run
//! the original circuit and the optimized one from the same random
//! state, compare via `QuantumRegister::fidelity`.

use crate::ir::{Circuit, Gate};
use std::collections::HashSet;

/// Runs the cancel/reorder passes to a fixed point.
pub fn optimize(circuit: &Circuit) -> Circuit {
    let mut gates = circuit.gates.clone();
    loop {
        let after_cancel = cancel_pass(&gates);
        let after_reorder = commute_forward_pass(&after_cancel);
        let after_cancel2 = cancel_pass(&after_reorder);
        if after_cancel2.len() == gates.len() {
            gates = after_cancel2;
            break;
        }
        gates = after_cancel2;
    }
    Circuit {
        num_qubits: circuit.num_qubits,
        num_clbits: circuit.num_clbits,
        gates,
    }
}

fn qubits_of(g: &Gate) -> Vec<usize> {
    g.qubits()
}

fn disjoint(a: &Gate, b: &Gate) -> bool {
    // `Measure(q, c)` is only disjoint-by-qubit-set from a gate on a
    // different qubit -- but two Measures writing different qubits
    // into the *same* classical bit `c` would be silently reordered by
    // that check alone, changing which one "wins" the bit. Treat any
    // gate touching a Measure as never disjoint, so it's never a
    // candidate for the commuting-reorder pass to slide past anything,
    // in either direction. Conservative on purpose: this crate does
    // not yet track classical-bit dependencies precisely enough to
    // reorder Measures safely (see `ir::Gate::Measure`'s doc comment).
    //
    // `If(clbit, ..)` gets the same treatment, for the same reason: it
    // *reads* a classical bit some earlier `Measure` wrote, and reads
    // conflict with writes exactly the way two writes to the same bit
    // do -- reordering it past the `Measure` that produced the value
    // it's conditioned on (or past another `If` reading the same bit)
    // could change what it observes, even though its own *qubit* set
    // may be disjoint from either. Same conservative call as `Measure`:
    // never a commute candidate, in either direction.
    if matches!(a, Gate::Measure(..) | Gate::If(..)) || matches!(b, Gate::Measure(..) | Gate::If(..)) {
        return false;
    }
    let qa: HashSet<usize> = qubits_of(a).into_iter().collect();
    let qb: HashSet<usize> = qubits_of(b).into_iter().collect();
    qa.is_disjoint(&qb)
}

/// The qubit a diagonal single-qubit gate acts on, or `None` if `g`
/// isn't one of the six diagonal single-qubit gates this module's rule
/// 1 and 2 apply to (see the module doc comment). `Rx`/`Ry`/`H`/`Measure`/
/// any two-qubit gate all return `None` here -- deliberately narrower
/// than "every `Gate` variant with one qubit argument", since e.g. `X`
/// and `Y` are single-qubit but *not* diagonal.
fn diagonal_single_qubit(g: &Gate) -> Option<usize> {
    match *g {
        Gate::Z(q) | Gate::S(q) | Gate::Sdg(q) | Gate::T(q) | Gate::Tdg(q) | Gate::Rz(q, _) => {
            Some(q)
        }
        _ => None,
    }
}

/// The qubit an X-basis single-qubit gate acts on (`X` or `Rx`), or `None` if `g`
/// isn't an X-basis single-qubit gate that rule 3 applies to.
fn x_basis_single_qubit(g: &Gate) -> Option<usize> {
    match *g {
        Gate::X(q) | Gate::Rx(q, _) => Some(q),
        _ => None,
    }
}

/// `true` if `a` immediately followed by `b` commute, checking `a`
/// as a diagonal single-qubit gate and `b` as the `Cx` (see [`commutes`]/this module's
/// doc comment for rule 1 and its derivation). Order-specific by design.
fn diagonal_commutes_through_cx_control(a: &Gate, b: &Gate) -> bool {
    match (diagonal_single_qubit(a), b) {
        (Some(q), Gate::Cx(control, _)) => q == *control,
        _ => false,
    }
}

/// `true` if `a` immediately followed by `b` commute, checking `a`
/// as an X-basis gate (`X` or `Rx`) and `b` as the `Cx` (see [`commutes`]/this module's doc
/// comment, rule 3, for the derivation). Order-specific by design.
fn x_basis_commutes_through_cx_target(a: &Gate, b: &Gate) -> bool {
    match (x_basis_single_qubit(a), b) {
        (Some(q), Gate::Cx(_, target)) => q == *target,
        _ => false,
    }
}

/// `true` if `a` immediately followed by `b` commute, checking `a` as
/// the diagonal single-qubit gate and `b` as the `Cz` (see
/// [`commutes`]/this module's doc comment, rule 2, for the derivation).
/// Order-specific by design. Unlike the `Cx`-control rule, this fires for
/// *either* of `Cz`'s two qubit arguments, since `Cz` is symmetric.
fn diagonal_commutes_through_cz(a: &Gate, b: &Gate) -> bool {
    match (diagonal_single_qubit(a), b) {
        (Some(q), Gate::Cz(x, y)) => q == *x || q == *y,
        _ => false,
    }
}

/// `true` if `a` immediately followed by `b` commute: either because
/// they're [`disjoint`], or because one of the three gate-specific
/// rules this module implements applies in either argument order (see
/// this module's doc comment). This is the check the commuting-reorder
/// pass below actually uses in place of a bare `disjoint` call.
///
/// `pub(crate)` (rather than private) so `crate::route`'s SABRE-style
/// router can reuse this exact, already-derived-and-tested predicate to
/// decide which gates sharing a wire are a genuine ordering constraint
/// versus which just happen to be adjacent in program order -- see
/// `route.rs`'s `build_commutation_predecessors` doc comment. No new
/// commutation rule is added here for that caller: this predicate only
/// covers single-qubit-gate-vs-two-qubit-gate pairs today (see this
/// module's doc comment), so a routing-relevant two-qubit/two-qubit
/// rule (e.g. `Cx(a,b)`/`Cx(a,c)` sharing a control) is deliberately
/// out of scope for this change and would need its own derivation and
/// test, same bar as rules 1-3 below.
pub(crate) fn commutes(a: &Gate, b: &Gate) -> bool {
    disjoint(a, b)
        || diagonal_commutes_through_cx_control(a, b)
        || diagonal_commutes_through_cx_control(b, a)
        || x_basis_commutes_through_cx_target(a, b)
        || x_basis_commutes_through_cx_target(b, a)
        || diagonal_commutes_through_cz(a, b)
        || diagonal_commutes_through_cz(b, a)
}

/// `true` if `a` immediately followed by `b` is the identity (same
/// qubits, mutually inverse operation).
fn is_inverse_pair(a: &Gate, b: &Gate) -> bool {
    use Gate::*;
    match (a, b) {
        // Self-inverse gates: two identical applications cancel.
        (H(q1), H(q2)) => q1 == q2,
        (X(q1), X(q2)) => q1 == q2,
        (Y(q1), Y(q2)) => q1 == q2,
        (Z(q1), Z(q2)) => q1 == q2,
        (Cx(a1, b1), Cx(a2, b2)) => a1 == a2 && b1 == b2,
        (Cz(a1, b1), Cz(a2, b2)) => (a1 == a2 && b1 == b2) || (a1 == b2 && b1 == a2),
        (Swap(a1, b1), Swap(a2, b2)) => (a1 == a2 && b1 == b2) || (a1 == b2 && b1 == a2),
        // Explicit inverse pairs.
        (S(q1), Sdg(q2)) | (Sdg(q1), S(q2)) => q1 == q2,
        (T(q1), Tdg(q2)) | (Tdg(q1), T(q2)) => q1 == q2,
        // Zero-angle rotations and angle-negating pairs on the same
        // qubit(s) also cancel; angle equality (not just presence)
        // matters here so this stays exact, not approximate.
        (Rx(q1, a1), Rx(q2, a2)) => q1 == q2 && (a1 + a2).abs() < 1e-12,
        (Ry(q1, a1), Ry(q2, a2)) => q1 == q2 && (a1 + a2).abs() < 1e-12,
        (Rz(q1, a1), Rz(q2, a2)) => q1 == q2 && (a1 + a2).abs() < 1e-12,
        (Rxx(a1, b1, t1), Rxx(a2, b2, t2))
        | (Ryy(a1, b1, t1), Ryy(a2, b2, t2))
        | (Rzz(a1, b1, t1), Rzz(a2, b2, t2)) => {
            ((a1 == a2 && b1 == b2) || (a1 == b2 && b1 == a2)) && (t1 + t2).abs() < 1e-12
        }
        (Cp(a1, b1, l1), Cp(a2, b2, l2)) => a1 == a2 && b1 == b2 && (l1 + l2).abs() < 1e-12,
        _ => false,
    }
}

/// Attempts to merge two single-qubit axis rotations acting on the same qubit.
/// Returns `Some(merged_gate)` if merged into a non-zero angle, or `None` if they
/// cancel completely (near zero angle) or cannot be merged.
fn try_merge_rotations(a: &Gate, b: &Gate) -> Option<Option<Gate>> {
    use Gate::*;
    match (a, b) {
        (Rx(q1, t1), Rx(q2, t2)) if q1 == q2 => {
            let total = t1 + t2;
            if total.abs() < 1e-12 {
                Some(None) // Complete cancellation
            } else {
                Some(Some(Rx(*q1, total)))
            }
        }
        (Ry(q1, t1), Ry(q2, t2)) if q1 == q2 => {
            let total = t1 + t2;
            if total.abs() < 1e-12 {
                Some(None) // Complete cancellation
            } else {
                Some(Some(Ry(*q1, total)))
            }
        }
        (Rz(q1, t1), Rz(q2, t2)) if q1 == q2 => {
            let total = t1 + t2;
            if total.abs() < 1e-12 {
                Some(None) // Complete cancellation
            } else {
                Some(Some(Rz(*q1, total)))
            }
        }
        _ => None,
    }
}

/// Removes every adjacent inverse pair, in one linear pass (a stack:
/// push a gate unless it cancels the top of the stack).
fn cancel_pass(gates: &[Gate]) -> Vec<Gate> {
    let mut stack: Vec<Gate> = Vec::with_capacity(gates.len());
    for g in gates {
        let cancels = stack
            .last()
            .map(|top| is_inverse_pair(top, g))
            .unwrap_or(false);
        if cancels {
            stack.pop();
        } else if let Some(top) = stack.last() {
            if let Some(merge_result) = try_merge_rotations(top, g) {
                stack.pop();
                if let Some(merged_gate) = merge_result {
                    stack.push(merged_gate);
                }
            } else {
                stack.push(g.clone());
            }
        } else {
            stack.push(g.clone());
        }
    }
    stack
}

/// For each gate, tries to slide it backward (earlier in the circuit)
/// past a run of gates it [`commutes`] with (disjoint qubits, or the
/// gate-specific rules this module implements -- see the module
/// doc comment), stopping as soon as it would land next to a gate it
/// can cancel/merge with, or hits a gate it can't pass. This is a single
/// forward scan that greedily pulls cancellable pairs together;
/// running it to a fixed point with `cancel_pass` (see `optimize`)
/// handles chains of reorders.
fn commute_forward_pass(gates: &[Gate]) -> Vec<Gate> {
    let mut out: Vec<Gate> = Vec::with_capacity(gates.len());
    for g in gates {
        // Find the furthest-back position we can slide g to: walk
        // backward from the end of `out` while each gate we pass
        // commutes with g. Stop early if we find a cancel or merge partner.
        let mut insert_at = out.len();
        let mut cancel_at: Option<usize> = None;
        let mut merged_gate: Option<Gate> = None;

        while insert_at > 0 {
            let candidate = &out[insert_at - 1];

            if is_inverse_pair(candidate, g) {
                cancel_at = Some(insert_at - 1);
                break;
            }

            if let Some(merge_result) = try_merge_rotations(candidate, g) {
                cancel_at = Some(insert_at - 1);
                merged_gate = merge_result;
                break;
            }

            if commutes(candidate, g) {
                insert_at -= 1;
                continue;
            }

            break;
        }

        match (cancel_at, merged_gate) {
            (Some(pos), Some(new_gate)) => {
                out[pos] = new_gate;
            }
            (Some(pos), None) => {
                out.remove(pos);
            }
            (None, _) => {
                out.insert(insert_at, g.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    // Validates `ir_optimize` two ways: exact-count checks for specific
    // cancellation patterns, and (for anything less trivial) a real
    // before/after fidelity comparison against
    // `sirraya_qutub::core::QuantumRegister`, same methodology as
    // `tests/decompositions.rs`.

    use rand::Rng;
    use sirraya_qutub::core::QuantumRegister;
    use crate::ir::{Circuit, Gate};
    use crate::ir_optimize::optimize as optimize_ir;

    const TOL: f64 = 1e-9;

    fn randomized_register(num_qubits: usize) -> QuantumRegister {
        let mut reg = QuantumRegister::new(num_qubits).unwrap();
        let mut rng = rand::thread_rng();
        for q in 0..num_qubits {
            reg.apply_rz(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
            reg.apply_ry(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
            reg.apply_rz(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
        }
        reg
    }

    fn apply_gate(reg: &mut QuantumRegister, g: &Gate) {
        match *g {
            Gate::H(q) => reg.apply_hadamard(q).unwrap(),
            Gate::X(q) => reg.apply_pauli_x(q).unwrap(),
            Gate::Y(q) => reg.apply_pauli_y(q).unwrap(),
            Gate::Z(q) => reg.apply_pauli_z(q).unwrap(),
            Gate::S(q) => reg.apply_s_gate(q).unwrap(),
            Gate::Sdg(q) => reg.apply_s_dag_gate(q).unwrap(),
            Gate::T(q) => reg.apply_t_gate(q).unwrap(),
            Gate::Tdg(q) => reg.apply_t_dag_gate(q).unwrap(),
            Gate::Rx(q, a) => reg.apply_rx(q, a).unwrap(),
            Gate::Ry(q, a) => reg.apply_ry(q, a).unwrap(),
            Gate::Rz(q, a) => reg.apply_rz(q, a).unwrap(),
            Gate::Cx(c, t) => reg.apply_cnot(c, t).unwrap(),
            Gate::Cz(c, t) => reg.apply_controlled_z(c, t).unwrap(),
            Gate::Swap(a, b) => reg.apply_swap(a, b).unwrap(),
            Gate::Rxx(a, b, t) => reg.apply_rxx(a, b, t).unwrap(),
            Gate::Ryy(a, b, t) => reg.apply_ryy(a, b, t).unwrap(),
            Gate::Rzz(a, b, t) => reg.apply_rzz(a, b, t).unwrap(),
            Gate::Cp(c, t, l) => reg.apply_controlled_phase(c, t, l).unwrap(),
            Gate::Measure(..) => panic!(
                "apply_gate: Measure has no fidelity-based test yet -- it needs the \
                 shot-based statistical methodology called for in the P0.1 roadmap item. \
                 No test in this file exercises Measure; this arm exists only to satisfy \
                 exhaustiveness."
            ),
            Gate::If(..) => panic!(
                "apply_gate: If has no fidelity-based test yet, for the same reason as \
                 Measure above -- it reads a classical bit this direct-simulation comparison \
                 has nowhere to produce. No test in this file exercises If; this arm exists \
                 only to satisfy exhaustiveness."
            ),
        }
    }

    fn assert_same_action(circuit: &Circuit, optimized: &Circuit) {
        let mut direct = randomized_register(circuit.num_qubits);
        let mut opt_reg = direct.clone();
        for g in &circuit.gates {
            apply_gate(&mut direct, g);
        }
        for g in &optimized.gates {
            apply_gate(&mut opt_reg, g);
        }
        let fidelity = direct.fidelity(&opt_reg).unwrap();
        assert!(
            (fidelity - 1.0).abs() < TOL,
            "optimized circuit doesn't match: fidelity {} (optimized: {:?})",
            fidelity,
            optimized.gates
        );
    }

    #[test]
    fn cancels_adjacent_cnot_pair() {
        let mut c = Circuit::new(2);
        c.push(Gate::H(0)).push(Gate::Cx(0, 1)).push(Gate::Cx(0, 1)).push(Gate::X(1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::H(0), Gate::X(1)]);
    }

    #[test]
    fn cancels_adjacent_swap_pair() {
        let mut c = Circuit::new(2);
        c.push(Gate::H(0)).push(Gate::Swap(0, 1)).push(Gate::Swap(1, 0));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::H(0)]);
    }

    #[test]
    fn cancels_s_sdg_pair() {
        let mut c = Circuit::new(1);
        c.push(Gate::S(0)).push(Gate::Sdg(0));
        let opt = optimize_ir(&c);
        assert!(opt.gates.is_empty());
    }

    #[test]
    fn commutes_past_disjoint_gate_to_cancel() {
        let mut c = Circuit::new(3);
        c.push(Gate::Cx(0, 1)).push(Gate::X(2)).push(Gate::Cx(0, 1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::X(2)]);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn does_not_commute_past_overlapping_gate() {
        let mut c = Circuit::new(3);
        c.push(Gate::Cx(0, 1)).push(Gate::Cx(1, 2)).push(Gate::Cx(0, 1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates.len(), 3, "no valid cancellation should have been found");
        assert_same_action(&c, &opt);
    }

    #[test]
    fn chain_of_commutes_finds_distant_cancellation() {
        let mut c = Circuit::new(4);
        c.push(Gate::Cx(0, 1))
            .push(Gate::H(2))
            .push(Gate::X(3))
            .push(Gate::Rz(2, 0.4))
            .push(Gate::Cx(0, 1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates.len(), 3, "the two CNOTs should have cancelled");
        assert_same_action(&c, &opt);
    }

    #[test]
    fn never_reorders_measure_past_a_qubit_disjoint_gate() {
        // X(1) is on a different qubit than Measure(0, 0), so it would
        // look "disjoint" by qubit alone -- but Measure must never be
        // sled past anything, so this must come out unchanged (not
        // reordered, and obviously not merged with anything).
        let mut c = Circuit::new(2);
        c.push(Gate::Measure(0, 0)).push(Gate::X(1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Measure(0, 0), Gate::X(1)]);
    }

    #[test]
    fn never_reorders_two_measures_writing_the_same_clbit() {
        // Measure(0, 0) then Measure(1, 0): different qubits, same
        // classical bit. Qubit-disjointness alone would let the
        // commuting pass treat these as swappable, silently changing
        // which measurement's outcome ends up in classical bit 0.
        let mut c = Circuit::new(2);
        c.push(Gate::Measure(0, 0)).push(Gate::Measure(1, 0));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Measure(0, 0), Gate::Measure(1, 0)]);
    }

    #[test]
    fn never_reorders_if_past_a_qubit_disjoint_gate() {
        // Same story as never_reorders_measure_past_a_qubit_disjoint_gate,
        // one level up: If(0, true, X(1)) reads clbit 0, and X(2) is on
        // a different qubit -- qubit-disjoint alone would make this
        // look swappable, but an If must never be slid past anything.
        let mut c = Circuit::new(3);
        c.num_clbits = 1;
        let if_gate = Gate::If(vec![(0, true)], Box::new(Gate::X(1)));
        c.push(if_gate.clone()).push(Gate::X(2));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![if_gate, Gate::X(2)]);
    }

    #[test]
    fn never_reorders_if_past_the_measure_it_depends_on() {
        // The exact teleportation shape: a Measure whose outcome an If
        // later reads. Even though If(0, ..) touches a different qubit
        // than Measure(0, 0), reordering them would let the correction
        // run before the outcome it's conditioned on has been produced.
        let mut c = Circuit::new(2);
        c.num_clbits = 1;
        let measure = Gate::Measure(0, 0);
        let if_gate = Gate::If(vec![(0, true)], Box::new(Gate::X(1)));
        c.push(measure.clone()).push(if_gate.clone());
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![measure, if_gate]);
    }

    #[test]
    fn commutes_rz_through_cx_control_wire_to_cancel() {
        // Rz(0, t) . Cx(0, 1) . Rz(0, -t): qubit 0 is Cx's control, so
        // the gate-specific rule should let the two Rz's commute
        // together across the Cx and cancel, leaving just the Cx.
        let mut c = Circuit::new(2);
        c.push(Gate::Rz(0, 0.4)).push(Gate::Cx(0, 1)).push(Gate::Rz(0, -0.4));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Cx(0, 1)]);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn does_not_commute_rz_through_cx_target_wire() {
        // Rz(1, t) . Cx(0, 1) . Rz(1, -t): qubit 1 is Cx's TARGET, not
        // its control -- this must NOT be treated as commuting or
        // cancelling. This mirrors backend.rs's exact distinction
        // (optimize_does_not_float_rz_through_cx_target_wire) at the
        // source level: naively cancelling here would silently drop a
        // real dependency, since this sandwich is not the identity.
        let mut c = Circuit::new(2);
        c.push(Gate::Rz(1, 0.4)).push(Gate::Cx(0, 1)).push(Gate::Rz(1, -0.4));
        let opt = optimize_ir(&c);
        assert_eq!(
            opt.gates.len(),
            3,
            "target-side Rz must not commute/cancel through Cx, got {:?}",
            opt.gates
        );
        assert_same_action(&c, &opt);
    }

    #[test]
    fn commutes_x_through_cx_target_wire_to_cancel() {
        // X(1) . Cx(0, 1) . X(1): qubit 1 is Cx's target, so rule 3
        // should let the two X's commute together across the Cx and
        // cancel, leaving just the Cx -- the mirror image of the
        // control-wire Rz case above.
        let mut c = Circuit::new(2);
        c.push(Gate::X(1)).push(Gate::Cx(0, 1)).push(Gate::X(1));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Cx(0, 1)]);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn does_not_commute_x_through_cx_control_wire() {
        // X(0) . Cx(0, 1) . X(0): qubit 0 is Cx's CONTROL, not its
        // target -- this must NOT be treated as commuting or
        // cancelling. Mirror image of does_not_commute_rz_through_cx_target_wire:
        // naively cancelling here would silently drop a real
        // dependency, since flipping the control changes which target
        // output Cx selects.
        let mut c = Circuit::new(2);
        c.push(Gate::X(0)).push(Gate::Cx(0, 1)).push(Gate::X(0));
        let opt = optimize_ir(&c);
        assert_eq!(
            opt.gates.len(),
            3,
            "control-side X must not commute/cancel through Cx, got {:?}",
            opt.gates
        );
        assert_same_action(&c, &opt);
    }

    #[test]
    fn control_side_rz_floats_through_cx_and_a_disjoint_gate_to_cancel() {
        // Rz(0, t) . Cx(0, 1) . X(2) . Rz(0, -t): the leading Rz(0,t)
        // has to cross both a Cx (via the new control-wire rule) and a
        // qubit-disjoint X(2) (via the existing disjoint rule) before
        // it can reach and cancel with the trailing Rz(0,-t) -- a
        // chain that exercises the two rules together.
        let mut c = Circuit::new(3);
        c.push(Gate::Rz(0, 0.4))
            .push(Gate::Cx(0, 1))
            .push(Gate::X(2))
            .push(Gate::Rz(0, -0.4));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates.len(), 2, "the two Rz's should have cancelled, got {:?}", opt.gates);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn commutes_rz_through_cz_either_wire_to_cancel() {
        // Rz(1, t) . Cz(0, 1) . Rz(1, -t): qubit 1 is one of Cz's two
        // (symmetric) wires, so the gate-specific rule should let the
        // two Rz's commute together across the Cz and cancel, leaving
        // just the Cz -- on *either* wire, unlike the Cx-control rule.
        let mut c = Circuit::new(2);
        c.push(Gate::Rz(1, 0.4)).push(Gate::Cz(0, 1)).push(Gate::Rz(1, -0.4));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Cz(0, 1)]);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn commutes_s_and_tdg_through_cz_on_the_other_wire_to_cancel() {
        // S(0) . Cz(0, 1) . Sdg(0): S/Sdg are diagonal and mutually
        // inverse, sitting on Cz's *other* wire from the Rz test above --
        // confirms the rule doesn't only fire for the qubit named first
        // in Cz's argument list.
        let mut c = Circuit::new(2);
        c.push(Gate::S(0)).push(Gate::Cz(0, 1)).push(Gate::Sdg(0));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Cz(0, 1)]);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn does_not_commute_non_diagonal_gate_through_cz() {
        // X(1) . Cz(0, 1) . X(1): X is single-qubit but NOT diagonal, so
        // it must not be treated as commuting/cancelling through Cz even
        // though it sits on one of Cz's wires and is self-inverse.
        let mut c = Circuit::new(2);
        c.push(Gate::X(1)).push(Gate::Cz(0, 1)).push(Gate::X(1));
        let opt = optimize_ir(&c);
        assert_eq!(
            opt.gates.len(),
            3,
            "non-diagonal X must not commute/cancel through Cz, got {:?}",
            opt.gates
        );
        assert_same_action(&c, &opt);
    }

    #[test]
    fn diagonal_gate_floats_through_cz_and_a_disjoint_gate_to_cancel() {
        // Rz(1, t) . Cz(0, 1) . X(2) . Rz(1, -t): the leading Rz(1, t)
        // has to cross both a Cz (via the new rule) and a qubit-disjoint
        // X(2) (via the existing disjoint rule) before it can reach and
        // cancel with the trailing Rz(1, -t) -- exercises the new rule
        // chained with an existing one, same shape as the analogous
        // Cx-control test above.
        let mut c = Circuit::new(3);
        c.push(Gate::Rz(1, 0.4))
            .push(Gate::Cz(0, 1))
            .push(Gate::X(2))
            .push(Gate::Rz(1, -0.4));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates.len(), 2, "the two Rz's should have cancelled, got {:?}", opt.gates);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn preserves_action_on_a_denser_mixed_circuit() {
        let mut c = Circuit::new(3);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 1))
            .push(Gate::T(2))
            .push(Gate::Cx(0, 1))
            .push(Gate::Swap(1, 2))
            .push(Gate::Swap(2, 1))
            .push(Gate::Rz(0, 0.3))
            .push(Gate::Rz(0, -0.3))
            .push(Gate::Cz(0, 2))
            .push(Gate::Y(1));
        let opt = optimize_ir(&c);
        assert!(
            opt.gates.len() < c.gates.len(),
            "expected some cancellation, got {} -> {} gates",
            c.gates.len(),
            opt.gates.len()
        );
        assert_same_action(&c, &opt);
    }

    #[test]
    fn does_not_commute_diagonal_gate_through_cx_target_wire() {
        // S(1) . Cx(0, 1) . Sdg(1): S/Sdg are diagonal, but diagonal
        // gates only commute through Cx on the CONTROL wire (rule 1)
        // -- qubit 1 here is the TARGET, where only X-basis gates
        // (rule 3) commute. Being an inverse pair isn't enough on its
        // own; they're not adjacent, and the widened rule 1 must not
        // be mistaken for applying to the target wire too.
        let mut c = Circuit::new(2);
        c.push(Gate::S(1)).push(Gate::Cx(0, 1)).push(Gate::Sdg(1));
        let opt = optimize_ir(&c);
        assert_eq!(
            opt.gates.len(),
            3,
            "target-side diagonal gate must not commute/cancel through Cx, got {:?}",
            opt.gates
        );
        assert_same_action(&c, &opt);
    }

    #[test]
    fn does_not_commute_rx_through_cx_control_wire() {
        // Rx(0, t) . Cx(0, 1) . Rx(0, -t): Rx is X-basis, but X-basis
        // gates only commute through Cx on the TARGET wire (rule 3)
        // -- qubit 0 here is the CONTROL, where only diagonal gates
        // (rule 1) commute. Mirror image of the test above: confirms
        // the widened rule 3 doesn't leak onto the control wire either.
        let mut c = Circuit::new(2);
        c.push(Gate::Rx(0, 0.4)).push(Gate::Cx(0, 1)).push(Gate::Rx(0, -0.4));
        let opt = optimize_ir(&c);
        assert_eq!(
            opt.gates.len(),
            3,
            "control-side X-basis gate must not commute/cancel through Cx, got {:?}",
            opt.gates
        );
        assert_same_action(&c, &opt);
    }

    #[test]
    fn commutes_rx_through_cx_target_wire_to_cancel() {
        let mut c = Circuit::new(2);
        c.push(Gate::Rx(1, 0.5)).push(Gate::Cx(0, 1)).push(Gate::Rx(1, -0.5));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Cx(0, 1)]);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn commutes_z_through_cx_control_wire_to_cancel() {
        let mut c = Circuit::new(2);
        c.push(Gate::Z(0)).push(Gate::Cx(0, 1)).push(Gate::Z(0));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates, vec![Gate::Cx(0, 1)]);
        assert_same_action(&c, &opt);
    }

    #[test]
    fn merges_adjacent_rz_rotations() {
        let mut c = Circuit::new(1);
        c.push(Gate::Rz(0, 0.2)).push(Gate::Rz(0, 0.3));
        let opt = optimize_ir(&c);
        assert_eq!(opt.gates.len(), 1);
        if let Gate::Rz(_, angle) = opt.gates[0] {
            assert!((angle - 0.5).abs() < 1e-12);
        } else {
            panic!("Expected Rz gate");
        }
        assert_same_action(&c, &opt);
    }
}