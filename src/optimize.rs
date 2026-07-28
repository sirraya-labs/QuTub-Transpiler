//! A small peephole optimizer over [`NativeCircuit`]s. Gate
//! decomposition (especially the fixed `Cx`/`Swap`/`Rxx`/`Ryy` identities
//! in [`crate::native`]) tends to emit adjacent same-axis rotations and
//! occasional zero-angle rotations; this pass merges and drops those
//! without changing the circuit's action (up to global phase, which is
//! never observable here -- see [`crate::native`]'s module doc).
//!
//! Two passes, run to a fixed point:
//! 1. **Merge**: adjacent `Rz`/`Ry` on the *same* qubit and *same* axis,
//!    with nothing else touching that qubit in between, combine their
//!    angles (`Rz(a) . Rz(b) == Rz(a+b)`, likewise `Ry`). Adjacent `Rzz`
//!    on the same qubit pair combine the same way.
//! 2. **Drop**: any gate whose combined angle is ~0 (mod 2*pi, since
//!    `Rz`/`Ry`/`Rzz` all have period 4*pi in the *matrix* but the
//!    circuits here never rely on that periodicity for correctness --
//!    only exact zero is dropped) is removed.

use crate::native::{NativeCircuit, NativeGate};

const EPS: f64 = 1e-9;
const TWO_PI: f64 = std::f64::consts::TAU;

pub fn optimize(circuit: &NativeCircuit) -> NativeCircuit {
    let mut gates = circuit.gates.clone();
    loop {
        let merged = merge_pass(&gates);
        let dropped = drop_zero_pass(&merged);
        if dropped.len() == gates.len() {
            gates = dropped;
            break;
        }
        gates = dropped;
    }
    NativeCircuit {
        num_qubits: circuit.num_qubits,
        num_clbits: circuit.num_clbits,
        gates,
    }
}

fn wrap_angle(theta: f64) -> f64 {
    // Keep angles in (-2*pi, 2*pi]; purely cosmetic, doesn't change the
    // gate (Rz/Ry/Rzz all only ever appear here as full, un-controlled
    // replacements, so a 2*pi shift is just an ignorable global phase --
    // see crate::native's module doc).
    let mut t = theta % TWO_PI;
    if t.abs() < EPS {
        t = 0.0;
    }
    t
}

fn merge_pass(gates: &[NativeGate]) -> Vec<NativeGate> {
    let mut out: Vec<NativeGate> = Vec::with_capacity(gates.len());
    for &g in gates {
        let merged = out.last().copied().and_then(|last| try_merge(last, g));
        match merged {
            Some(combined) => {
                out.pop();
                out.push(combined);
            }
            None => out.push(g),
        }
    }
    out
}

/// Merges `g` into the immediately preceding gate `last` if they act on
/// the same qubit(s) and axis -- i.e. nothing in between could have
/// blocked commuting them together, because they're already adjacent.
fn try_merge(last: NativeGate, g: NativeGate) -> Option<NativeGate> {
    match (last, g) {
        (NativeGate::Rz(q1, a1), NativeGate::Rz(q2, a2)) if q1 == q2 => {
            Some(NativeGate::Rz(q1, wrap_angle(a1 + a2)))
        }
        (NativeGate::Ry(q1, a1), NativeGate::Ry(q2, a2)) if q1 == q2 => {
            Some(NativeGate::Ry(q1, wrap_angle(a1 + a2)))
        }
        (NativeGate::Rzz(a1, b1, t1), NativeGate::Rzz(a2, b2, t2))
            if (a1 == a2 && b1 == b2) || (a1 == b2 && b1 == a2) =>
        {
            Some(NativeGate::Rzz(a1, b1, wrap_angle(t1 + t2)))
        }
        _ => None,
    }
}

fn drop_zero_pass(gates: &[NativeGate]) -> Vec<NativeGate> {
    gates
        .iter()
        .copied()
        .filter(|g| match g {
            NativeGate::Rz(_, a) | NativeGate::Ry(_, a) => a.abs() > EPS,
            NativeGate::Rzz(_, _, a) => a.abs() > EPS,
            // Never a candidate for dropping: it isn't a rotation with
            // an "angle" to net to zero, and it's a real classical side
            // effect the caller depends on.
            NativeGate::Measure(..) => true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_adjacent_same_axis_rotations() {
        let mut nc = NativeCircuit::new(1);
        nc.push(NativeGate::Rz(0, 0.3));
        nc.push(NativeGate::Rz(0, 0.4));
        let opt = optimize(&nc);
        assert_eq!(opt.gates, vec![NativeGate::Rz(0, wrap_angle(0.7))]);
    }

    #[test]
    fn drops_gates_that_cancel_to_zero() {
        let mut nc = NativeCircuit::new(1);
        nc.push(NativeGate::Ry(0, 0.5));
        nc.push(NativeGate::Ry(0, -0.5));
        let opt = optimize(&nc);
        assert!(opt.gates.is_empty());
    }

    #[test]
    fn does_not_merge_across_a_different_qubit_gate() {
        let mut nc = NativeCircuit::new(2);
        nc.push(NativeGate::Rz(0, 0.3));
        nc.push(NativeGate::Rz(1, 0.1));
        nc.push(NativeGate::Rz(0, 0.4));
        let opt = optimize(&nc);
        assert_eq!(opt.gates.len(), 3, "gates on q0 aren't adjacent, shouldn't merge");
    }

    #[test]
    fn never_drops_or_merges_measure() {
        let mut nc = NativeCircuit::new(1);
        nc.push(NativeGate::Rz(0, 0.0)); // would be dropped on its own
        nc.push(NativeGate::Measure(0, 0));
        let opt = optimize(&nc);
        assert_eq!(opt.gates, vec![NativeGate::Measure(0, 0)]);
    }

    #[test]
    fn merges_rzz_regardless_of_qubit_order() {
        let mut nc = NativeCircuit::new(2);
        nc.push(NativeGate::Rzz(0, 1, 0.2));
        nc.push(NativeGate::Rzz(1, 0, 0.3));
        let opt = optimize(&nc);
        assert_eq!(opt.gates, vec![NativeGate::Rzz(0, 1, wrap_angle(0.5))]);
    }
}
