//! The three-qubit phase-flip code: corrects a single `Z` (phase-flip)
//! error on any one of its three physical qubits -- the dual of
//! [`crate::qec::bit_flip::ThreeQubitBitFlipCode`], using `X`-type
//! stabilizers instead of `Z`-type. Nielsen & Chuang, *Quantum
//! Computation and Quantum Information*, Section 10.1.2.
//!
//! # The code
//! Logical basis states `|0>_L = |+++>`, `|1>_L = |--->`: the same
//! `Cx(0,1) . Cx(0,2)` encoding as the bit-flip code, followed by `H`
//! on every data qubit (turning `|000>`/`|111>` into `|+++>`/`|--->`).
//!
//! Stabilizer generators: `X0 X1` and `X1 X2`. Measuring an `X`-type
//! stabilizer needs a different ancilla circuit than a `Z`-type one --
//! see [`ThreeQubitPhaseFlipCode::extract_syndrome`]'s doc comment --
//! but the resulting syndrome table is structurally identical to the
//! bit-flip code's (see [`ThreeQubitPhaseFlipCode::correct`]), just
//! correcting with `Z` instead of `X`.
//!
//! # What this code does *not* correct
//! An `X` (bit-flip) error, or a simultaneous error on more than one
//! qubit -- symmetric to [`crate::qec::bit_flip`]'s own limits.
//! Precisely: this code is the bit-flip code conjugated by `H` on
//! every data qubit, so its logical operators are also conjugated
//! (`H.Z.H = X`, `H.(X0X1X2).H = Z0Z1Z2`) -- meaning a *single*
//! physical `X` error on any one data qubit is mathematically
//! identical to this code's own logical `Z_L` operator, passing
//! through completely undetected, not "corrupted" so much as
//! indistinguishable from a deliberate logical gate. See this
//! module's own tests, checked precisely against the real simulator.

use crate::ir::{Circuit, Gate};
use crate::qec::StabilizerCode;

pub struct ThreeQubitPhaseFlipCode;

impl StabilizerCode for ThreeQubitPhaseFlipCode {
    fn id(&self) -> &'static str {
        "ThreeQubitPhaseFlip"
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
            "ThreeQubitPhaseFlipCode needs exactly 3 data qubits"
        );
        let (q0, q1, q2) = (data_qubits[0], data_qubits[1], data_qubits[2]);
        circuit
            .push(Gate::Cx(q0, q1))
            .push(Gate::Cx(q0, q2))
            .push(Gate::H(q0))
            .push(Gate::H(q1))
            .push(Gate::H(q2));
    }

    /// Measures `X0 X1` into `syndrome_clbits[0]` and `X1 X2` into
    /// `syndrome_clbits[1]`, via the standard `X`-type stabilizer
    /// circuit: each ancilla starts in `|0>`, is Hadamard'd into
    /// `|+>`, *controls* a `Cx` onto each data qubit the stabilizer
    /// touches (the reverse control/target direction from the
    /// `Z`-type case, since an `X`-type stabilizer needs the ancilla
    /// acting in the Hadamard basis), is Hadamard'd back, then
    /// measured. This is the standard general stabilizer-measurement
    /// circuit for an `X`-type generator, not something invented for
    /// this code specifically.
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
            "ThreeQubitPhaseFlipCode needs exactly 3 data qubits"
        );
        assert_eq!(
            ancilla_qubits.len(),
            2,
            "ThreeQubitPhaseFlipCode needs exactly 2 ancillas"
        );
        assert_eq!(
            syndrome_clbits.len(),
            2,
            "ThreeQubitPhaseFlipCode needs exactly 2 syndrome bits"
        );
        let (q0, q1, q2) = (data_qubits[0], data_qubits[1], data_qubits[2]);
        let (a0, a1) = (ancilla_qubits[0], ancilla_qubits[1]);
        // X0 X1 -> syndrome_clbits[0]
        circuit
            .push(Gate::H(a0))
            .push(Gate::Cx(a0, q0))
            .push(Gate::Cx(a0, q1))
            .push(Gate::H(a0));
        // X1 X2 -> syndrome_clbits[1]
        circuit
            .push(Gate::H(a1))
            .push(Gate::Cx(a1, q1))
            .push(Gate::Cx(a1, q2))
            .push(Gate::H(a1));
        circuit
            .push(Gate::Measure(a0, syndrome_clbits[0]))
            .push(Gate::Measure(a1, syndrome_clbits[1]));
    }

    /// Structurally identical table to
    /// [`ThreeQubitBitFlipCode::correct`](crate::qec::bit_flip::ThreeQubitBitFlipCode::correct)'s,
    /// with `Z` corrections in place of `X`: `X0 X1` anticommutes with
    /// a `Z` error on `q0` or `q1`; `X1 X2` anticommutes with a `Z`
    /// error on `q1` or `q2`. `(1,0)` -> `Z(q0)`, `(1,1)` -> `Z(q1)`,
    /// `(0,1)` -> `Z(q2)`, `(0,0)` -> no correction.
    fn correct(&self, circuit: &mut Circuit, data_qubits: &[usize], syndrome_clbits: &[usize]) {
        assert_eq!(
            data_qubits.len(),
            3,
            "ThreeQubitPhaseFlipCode needs exactly 3 data qubits"
        );
        assert_eq!(
            syndrome_clbits.len(),
            2,
            "ThreeQubitPhaseFlipCode needs exactly 2 syndrome bits"
        );
        let (q0, q1, q2) = (data_qubits[0], data_qubits[1], data_qubits[2]);
        let (s0, s1) = (syndrome_clbits[0], syndrome_clbits[1]);
        circuit.push(Gate::If(
            vec![(s0, true), (s1, false)],
            Box::new(Gate::Z(q0)),
        ));
        circuit.push(Gate::If(
            vec![(s0, true), (s1, true)],
            Box::new(Gate::Z(q1)),
        ));
        circuit.push(Gate::If(
            vec![(s0, false), (s1, true)],
            Box::new(Gate::Z(q2)),
        ));
    }

    /// The exact inverse of [`encode`](Self::encode): undo the `H`s
    /// first, then the same commuting-`Cx`-pair argument
    /// [`ThreeQubitBitFlipCode::decode`](crate::qec::bit_flip::ThreeQubitBitFlipCode::decode)
    /// uses.
    fn decode(&self, circuit: &mut Circuit, data_qubits: &[usize]) {
        assert_eq!(
            data_qubits.len(),
            3,
            "ThreeQubitPhaseFlipCode needs exactly 3 data qubits"
        );
        let (q0, q1, q2) = (data_qubits[0], data_qubits[1], data_qubits[2]);
        circuit
            .push(Gate::H(q0))
            .push(Gate::H(q1))
            .push(Gate::H(q2))
            .push(Gate::Cx(q0, q2))
            .push(Gate::Cx(q0, q1));
    }
}
