//! The three-qubit bit-flip code: corrects a single `X` (bit-flip)
//! error on any one of its three physical qubits. Shor, *Phys. Rev. A*
//! 52, R2493 (1995); Nielsen & Chuang, *Quantum Computation and
//! Quantum Information*, Section 10.1.
//!
//! # The code
//! Logical basis states `|0>_L = |000>`, `|1>_L = |111>` -- an
//! arbitrary logical state `a|0>_L + b|1>_L` is prepared from
//! `a|0> + b|1>` on `data_qubits[0]` via `Cx(0,1) . Cx(0,2)`.
//!
//! Stabilizer generators: `Z0 Z1` and `Z1 Z2` (0-indexed;
//! `Z1 Z2`/`Z2 Z3` in Nielsen & Chuang's 1-indexed notation). Both
//! stabilize the codespace (`+1` eigenvalue on both `|000>` and
//! `|111>`), and together they distinguish which single qubit (if any)
//! a bit-flip error landed on -- see [`ThreeQubitBitFlipCode::correct`]'s
//! doc comment for the exact syndrome table this implies.
//!
//! # What this code does *not* correct
//! A `Z` (phase-flip) error -- see [`crate::qec::phase_flip`] for the
//! dual code that handles it -- or a simultaneous error on more than
//! one qubit. Precisely: a *single* physical `Z` error on any one
//! data qubit is mathematically identical to this code's own logical
//! `Z_L` operator (`Zi|000> = |000>`, `Zi|111> = -|111>`, exactly the
//! action a logical `Z` should have), so it passes through completely
//! undetected -- not "corrupted," but indistinguishable from a
//! deliberate logical gate. Two simultaneous `X` errors similarly
//! aren't detected as *two* errors; they produce the same syndrome as
//! some other single-qubit `X` error, so the decoder's "correction"
//! combines with the real errors into a net logical `X_L` flip
//! (`X_L = X0 X1 X2`, since flipping every data qubit maps `|000>` to
//! `|111>` and back). See this module's own tests for both, checked
//! precisely against the real simulator rather than just asserted.

use crate::ir::{Circuit, Gate};
use crate::qec::StabilizerCode;

pub struct ThreeQubitBitFlipCode;

impl StabilizerCode for ThreeQubitBitFlipCode {
    fn id(&self) -> &'static str {
        "ThreeQubitBitFlip"
    }

    fn num_data_qubits(&self) -> usize {
        3
    }

    fn num_syndrome_bits(&self) -> usize {
        2
    }

    fn encode(&self, circuit: &mut Circuit, data_qubits: &[usize]) {
        assert_eq!(
            data_qubits.len(),
            3,
            "ThreeQubitBitFlipCode needs exactly 3 data qubits"
        );
        let (q0, q1, q2) = (data_qubits[0], data_qubits[1], data_qubits[2]);
        circuit.push(Gate::Cx(q0, q1)).push(Gate::Cx(q0, q2));
    }

    /// Measures `Z0 Z1` into `syndrome_clbits[0]` and `Z1 Z2` into
    /// `syndrome_clbits[1]`, via the standard `Z`-type stabilizer
    /// circuit: each ancilla starts in `|0>`, is the *target* of
    /// `Cx` from each data qubit the stabilizer touches (parity onto
    /// the ancilla, no Hadamards needed since `Z⊗Z` is already diagonal
    /// in the computational basis both the ancilla and data qubits
    /// start in), then measured directly.
    fn extract_syndrome(
        &self,
        circuit: &mut Circuit,
        data_qubits: &[usize],
        ancilla_qubits: &[usize],
        syndrome_clbits: &[usize],
    ) {
        assert_eq!(
            data_qubits.len(),
            3,
            "ThreeQubitBitFlipCode needs exactly 3 data qubits"
        );
        assert_eq!(
            ancilla_qubits.len(),
            2,
            "ThreeQubitBitFlipCode needs exactly 2 ancillas"
        );
        assert_eq!(
            syndrome_clbits.len(),
            2,
            "ThreeQubitBitFlipCode needs exactly 2 syndrome bits"
        );
        let (q0, q1, q2) = (data_qubits[0], data_qubits[1], data_qubits[2]);
        let (a0, a1) = (ancilla_qubits[0], ancilla_qubits[1]);
        // Z0 Z1 -> syndrome_clbits[0]
        circuit.push(Gate::Cx(q0, a0)).push(Gate::Cx(q1, a0));
        // Z1 Z2 -> syndrome_clbits[1]
        circuit.push(Gate::Cx(q1, a1)).push(Gate::Cx(q2, a1));
        circuit
            .push(Gate::Measure(a0, syndrome_clbits[0]))
            .push(Gate::Measure(a1, syndrome_clbits[1]));
    }

    /// The syndrome table this code's two stabilizers imply: `Z0 Z1`
    /// anticommutes with (and so flips the sign measured by) an `X`
    /// error on `q0` or `q1`; `Z1 Z2` anticommutes with an `X` error on
    /// `q1` or `q2`. So a single `X` error on:
    /// - `q0` flips only the first syndrome bit: `(1, 0)`.
    /// - `q1` flips both (it's in both stabilizers): `(1, 1)`.
    /// - `q2` flips only the second: `(0, 1)`.
    /// - no error: `(0, 0)`, no correction.
    ///
    /// Each of the three nontrivial cases is exactly one
    /// [`crate::ir::Gate::If`] with a two-condition AND over
    /// `syndrome_clbits` -- the real thing this code exists to
    /// demonstrate (see `qec/mod.rs`'s own doc comment).
    fn correct(&self, circuit: &mut Circuit, data_qubits: &[usize], syndrome_clbits: &[usize]) {
        assert_eq!(
            data_qubits.len(),
            3,
            "ThreeQubitBitFlipCode needs exactly 3 data qubits"
        );
        assert_eq!(
            syndrome_clbits.len(),
            2,
            "ThreeQubitBitFlipCode needs exactly 2 syndrome bits"
        );
        let (q0, q1, q2) = (data_qubits[0], data_qubits[1], data_qubits[2]);
        let (s0, s1) = (syndrome_clbits[0], syndrome_clbits[1]);
        circuit.push(Gate::If(
            vec![(s0, true), (s1, false)],
            Box::new(Gate::X(q0)),
        ));
        circuit.push(Gate::If(
            vec![(s0, true), (s1, true)],
            Box::new(Gate::X(q1)),
        ));
        circuit.push(Gate::If(
            vec![(s0, false), (s1, true)],
            Box::new(Gate::X(q2)),
        ));
    }

    /// The exact inverse of [`encode`](Self::encode) -- `Cx(0,1)` and
    /// `Cx(0,2)` are each their own inverse, and the two commute (same
    /// control, different targets), so re-applying them in either
    /// order undoes the encoding.
    fn decode(&self, circuit: &mut Circuit, data_qubits: &[usize]) {
        assert_eq!(
            data_qubits.len(),
            3,
            "ThreeQubitBitFlipCode needs exactly 3 data qubits"
        );
        let (q0, q1, q2) = (data_qubits[0], data_qubits[1], data_qubits[2]);
        circuit.push(Gate::Cx(q0, q2)).push(Gate::Cx(q0, q1));
    }
}
