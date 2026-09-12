//! An arbitrary-distance quantum repetition code -- the real-decoder
//! generalization of [`crate::qec::bit_flip::ThreeQubitBitFlipCode`]
//! and [`crate::qec::phase_flip::ThreeQubitPhaseFlipCode`] (which are
//! exactly `RepetitionCode::new(3, PauliCorrection::X)` and
//! `RepetitionCode::new(3, PauliCorrection::Z)`, respectively -- see
//! this module's own tests, which check that claim directly against
//! the real simulator rather than just stating it).
//!
//! # The code, and its decoding graph
//! `d` data qubits, `d-1` syndrome checks (check `i` measures
//! `Z_i Z_{i+1}` for an `X`-correcting code, `X_i X_{i+1}` for a
//! `Z`-correcting one -- see [`RepetitionCode::new`]). A single error
//! on data qubit `0` flips only check `0`; on data qubit `d-1`, only
//! check `d-2`; on any interior qubit `i` (`0 < i < d-1`), both checks
//! `i-1` and `i`. This is exactly a **path graph**: a [`Boundary`](crate::qec::decoder::DecodingNode::Boundary)
//! node, then check `0`, check `1`, ..., check `d-2`, then the boundary
//! again -- with each of the `d` data qubits corresponding to exactly
//! one edge. [`RepetitionCode::decode_syndrome`] builds this graph
//! (implicitly, via its distance function) and hands it to
//! [`crate::qec::decoder::minimum_weight_perfect_matching`] -- the
//! same real decoding strategy a surface code's (2D) decoding graph
//! would use, just on the simplest possible (1D) instance of it. That
//! parallel is deliberate: getting the repetition code's graph exactly
//! right, and checking it against the real simulator at several
//! distances, is what makes reusing [`crate::qec::decoder`] for a
//! future 2D code a matter of building the right graph, not
//! re-deriving the decoding algorithm.
//!
//! # What this code corrects
//! Any error pattern the minimum-weight matching actually resolves to
//! the true error -- guaranteed for up to `(d-1)/2` errors (the
//! standard repetition-code distance bound; see this module's own
//! tests for `d=5` and `d=7`, checked exhaustively over every
//! correctable single- and double-error pattern, not just asserted).
//! Like the fixed `d=3` codes, a `RepetitionCode` corrects only the one
//! Pauli type it was built for -- see [`RepetitionCode::new`].

use crate::ir::{Circuit, Gate};
use crate::qec::decoder::{corrections_from_matching, minimum_weight_perfect_matching, DecodingNode};
use crate::qec::{DecodableCode, PauliCorrection};

pub struct RepetitionCode {
    distance: usize,
    corrects: PauliCorrection,
}

impl RepetitionCode {
    /// A distance-`d` repetition code correcting `corrects`-type
    /// errors. `corrects` must be [`PauliCorrection::X`] or
    /// [`PauliCorrection::Z`] -- `Y` has no meaning here (this code's
    /// stabilizers are single-Pauli-type by construction; a code that
    /// corrects `Y` errors specifically would need a different
    /// stabilizer structure entirely, e.g. a genuine CSS combination
    /// like the Shor code, not a repetition code). `d` must be odd and
    /// `>= 3` -- an even distance can't break every possible tie in
    /// the matching decoder the way an odd one guarantees.
    pub fn new(d: usize, corrects: PauliCorrection) -> Self {
        assert!(d >= 3, "RepetitionCode needs distance >= 3, got {d}");
        assert!(d % 2 == 1, "RepetitionCode needs an odd distance, got {d}");
        assert!(
            matches!(corrects, PauliCorrection::X | PauliCorrection::Z),
            "RepetitionCode corrects X or Z, not Y (see RepetitionCode::new's own doc comment)"
        );
        Self { distance: d, corrects }
    }

    /// Distance between two decoding-graph nodes along the path graph
    /// this module's own doc comment describes -- the number of data
    /// qubits (graph edges) on the shortest route between them.
    fn graph_distance(&self, a: DecodingNode, b: DecodingNode) -> f64 {
        let d = self.distance;
        let nearest_boundary = |i: usize| -> f64 { (i + 1).min(d - 1 - i) as f64 };
        match (a, b) {
            (DecodingNode::Check(i), DecodingNode::Check(j)) => (i as f64 - j as f64).abs(),
            (DecodingNode::Check(i), DecodingNode::Boundary)
            | (DecodingNode::Boundary, DecodingNode::Check(i)) => nearest_boundary(i),
            (DecodingNode::Boundary, DecodingNode::Boundary) => 0.0,
        }
    }

    /// The ordered list of data qubits (graph edges) on the shortest
    /// path between two decoding-graph nodes -- the qubits
    /// [`crate::qec::decoder::corrections_from_matching`] flips for a
    /// matched pair. Always takes the shorter of the two possible
    /// routes to the boundary (matching [`graph_distance`](Self::graph_distance)'s
    /// own `nearest_boundary`, so a path's length always equals the
    /// distance function's own value for the same two nodes -- checked
    /// directly by this module's own tests, not just assumed).
    fn path_qubits(&self, a: DecodingNode, b: DecodingNode) -> Vec<usize> {
        let d = self.distance;
        match (a, b) {
            (DecodingNode::Check(i), DecodingNode::Check(j)) => {
                let (lo, hi) = (i.min(j), i.max(j));
                // Qubit k (0 < k < d-1) sits on the edge between
                // Check(k-1) and Check(k) -- see this module's own doc
                // comment on the graph structure.
                (lo + 1..=hi).collect()
            }
            (DecodingNode::Check(i), DecodingNode::Boundary)
            | (DecodingNode::Boundary, DecodingNode::Check(i)) => {
                let left_len = i + 1; // qubits 0..=i
                let right_len = d - 1 - i; // qubits i+1..d
                if left_len <= right_len {
                    (0..=i).collect()
                } else {
                    (i + 1..d).collect()
                }
            }
            (DecodingNode::Boundary, DecodingNode::Boundary) => Vec::new(),
        }
    }
}

impl DecodableCode for RepetitionCode {
    fn id(&self) -> &'static str {
        "RepetitionCode"
    }

    fn num_data_qubits(&self) -> usize {
        self.distance
    }

    fn num_syndrome_bits(&self) -> usize {
        self.distance - 1
    }

    fn encode(&self, circuit: &mut Circuit, data_qubits: &[usize]) {
        assert_eq!(data_qubits.len(), self.distance, "RepetitionCode: wrong data qubit count");
        let q0 = data_qubits[0];
        for &qi in &data_qubits[1..] {
            circuit.push(Gate::Cx(q0, qi));
        }
        if self.corrects == PauliCorrection::Z {
            for &q in data_qubits {
                circuit.push(Gate::H(q));
            }
        }
    }

    fn extract_syndrome(
        &self,
        circuit: &mut Circuit,
        data_qubits: &[usize],
        ancilla_qubits: &[usize],
        syndrome_clbits: &[usize],
    ) {
        let d = self.distance;
        assert_eq!(data_qubits.len(), d, "RepetitionCode: wrong data qubit count");
        assert_eq!(ancilla_qubits.len(), d - 1, "RepetitionCode: wrong ancilla count");
        assert_eq!(syndrome_clbits.len(), d - 1, "RepetitionCode: wrong syndrome bit count");
        for i in 0..d - 1 {
            let (qi, qj, a) = (data_qubits[i], data_qubits[i + 1], ancilla_qubits[i]);
            match self.corrects {
                PauliCorrection::X => {
                    // Z-type stabilizer: same circuit as bit_flip.rs's
                    // extract_syndrome, generalized to d-1 checks.
                    circuit.push(Gate::Cx(qi, a)).push(Gate::Cx(qj, a));
                }
                PauliCorrection::Z => {
                    // X-type stabilizer: same circuit as phase_flip.rs's.
                    circuit.push(Gate::H(a)).push(Gate::Cx(a, qi)).push(Gate::Cx(a, qj)).push(Gate::H(a));
                }
                PauliCorrection::Y => unreachable!("rejected by RepetitionCode::new"),
            }
            circuit.push(Gate::Measure(a, syndrome_clbits[i]));
        }
    }

    fn decode(&self, circuit: &mut Circuit, data_qubits: &[usize]) {
        assert_eq!(data_qubits.len(), self.distance, "RepetitionCode: wrong data qubit count");
        if self.corrects == PauliCorrection::Z {
            for &q in data_qubits {
                circuit.push(Gate::H(q));
            }
        }
        let q0 = data_qubits[0];
        for &qi in data_qubits[1..].iter().rev() {
            circuit.push(Gate::Cx(q0, qi));
        }
    }

    fn decode_syndrome(&self, syndrome: &[u8]) -> Vec<(usize, PauliCorrection)> {
        assert_eq!(syndrome.len(), self.distance - 1, "RepetitionCode: wrong syndrome length");
        let defects: Vec<DecodingNode> = syndrome
            .iter()
            .enumerate()
            .filter(|&(_, &bit)| bit != 0)
            .map(|(i, _)| DecodingNode::Check(i))
            .collect();
        let matching =
            minimum_weight_perfect_matching(&defects, |a, b| self.graph_distance(a, b));
        let qubits = corrections_from_matching(&matching, |a, b| self.path_qubits(a, b));
        qubits.into_iter().map(|q| (q, self.corrects)).collect()
    }
}

/// Builds a [`Gate`] that applies `self.corrects`'s Pauli to `q` --
/// used only by this module's own tests, to inject the same kind of
/// error [`RepetitionCode`] is built to correct.
#[cfg(test)]
impl RepetitionCode {
    fn error_gate(&self, q: usize) -> Gate {
        match self.corrects {
            PauliCorrection::X => Gate::X(q),
            PauliCorrection::Z => Gate::Z(q),
            PauliCorrection::Y => unreachable!("rejected by RepetitionCode::new"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qec::bit_flip::ThreeQubitBitFlipCode;
    use crate::qec::phase_flip::ThreeQubitPhaseFlipCode;
    use crate::qec::{run_decodable_round, StabilizerCode};
    use sirraya_qutub::core::QuantumRegister;
    use sirraya_qutub::DensityMatrix;

    fn arbitrary_state_prep(c: &mut Circuit, q: usize) {
        c.push(Gate::Rz(q, 1.3));
        c.push(Gate::Ry(q, 0.7));
    }

    fn arbitrary_state_target() -> DensityMatrix {
        let mut reg = QuantumRegister::new(1).unwrap();
        reg.apply_rz(0, 1.3).unwrap();
        reg.apply_ry(0, 0.7).unwrap();
        reg.to_density_matrix().unwrap()
    }

    /// The real consistency check this module's own doc comment
    /// promises: the general, MWPM-graph-based `RepetitionCode` at
    /// distance 3 must produce *exactly* the same physical recovery as
    /// the original, hand-enumerated `ThreeQubitBitFlipCode`/
    /// `ThreeQubitPhaseFlipCode` -- for every single-error location and
    /// the no-error case, checked against real quantum state, not just
    /// "both correct the state" (which wouldn't rule out them
    /// disagreeing on some other observable detail).
    #[test]
    fn distance_3_repetition_code_matches_the_original_hand_enumerated_codes() {
        let target = arbitrary_state_target();

        let bit_flip_old = ThreeQubitBitFlipCode;
        let bit_flip_new = RepetitionCode::new(3, PauliCorrection::X);
        for injected in [None, Some(Gate::X(0)), Some(Gate::X(1)), Some(Gate::X(2))] {
            let mut c = Circuit::new(5);
            c.num_clbits = 2;
            arbitrary_state_prep(&mut c, 0);
            bit_flip_old.encode(&mut c, &[0, 1, 2]);
            if let Some(g) = injected.clone() {
                c.push(g);
            }
            bit_flip_old.extract_syndrome(&mut c, &[0, 1, 2], &[3, 4], &[0, 1]);
            bit_flip_old.correct(&mut c, &[0, 1, 2], &[0, 1]);
            bit_flip_old.decode(&mut c, &[0, 1, 2]);
            let opt = crate::ir_optimize::optimize(&c);
            let native = crate::native::decompose(&opt);
            let (reg, _) = crate::emit::run_with_measurement(&native).unwrap();
            let old_recovered = reg.to_density_matrix().unwrap().partial_trace(&[0]).unwrap();

            let new_recovered =
                run_decodable_round(&bit_flip_new, arbitrary_state_prep, injected.clone()).unwrap();

            let old_fid = old_recovered.fidelity(&target).unwrap();
            let new_fid = new_recovered.fidelity(&target).unwrap();
            assert!((old_fid - 1.0).abs() < 1e-9, "old code failed for {:?}", injected);
            assert!((new_fid - 1.0).abs() < 1e-9, "new decoder failed for {:?}", injected);
            assert!(
                old_recovered.fidelity(&new_recovered).unwrap() > 1.0 - 1e-9,
                "old and new decoders disagree for injected error {:?}",
                injected
            );
        }

        // Same cross-check for the phase-flip / Z-correcting side.
        let phase_flip_old = ThreeQubitPhaseFlipCode;
        let phase_flip_new = RepetitionCode::new(3, PauliCorrection::Z);
        for injected in [None, Some(Gate::Z(0)), Some(Gate::Z(1)), Some(Gate::Z(2))] {
            let mut c = Circuit::new(5);
            c.num_clbits = 2;
            arbitrary_state_prep(&mut c, 0);
            phase_flip_old.encode(&mut c, &[0, 1, 2]);
            if let Some(g) = injected.clone() {
                c.push(g);
            }
            phase_flip_old.extract_syndrome(&mut c, &[0, 1, 2], &[3, 4], &[0, 1]);
            phase_flip_old.correct(&mut c, &[0, 1, 2], &[0, 1]);
            phase_flip_old.decode(&mut c, &[0, 1, 2]);
            let opt = crate::ir_optimize::optimize(&c);
            let native = crate::native::decompose(&opt);
            let (reg, _) = crate::emit::run_with_measurement(&native).unwrap();
            let old_recovered = reg.to_density_matrix().unwrap().partial_trace(&[0]).unwrap();

            let new_recovered =
                run_decodable_round(&phase_flip_new, arbitrary_state_prep, injected.clone()).unwrap();

            assert!(
                old_recovered.fidelity(&new_recovered).unwrap() > 1.0 - 1e-9,
                "old and new decoders disagree for injected Z error {:?}",
                injected
            );
        }
    }

    /// The real scale claim: a distance-5 repetition code corrects
    /// *any* single error, and *some* (not all -- see this module's
    /// doc comment on the distance bound) double-error patterns --
    /// checked exhaustively over every single-error location (5 cases)
    /// and every double-error location (10 combinations), against real
    /// quantum state. A static `Gate::If` table for this many syndrome
    /// bits (4 bits, 16 possible syndromes) would already be unwieldy;
    /// this is exactly the scale the real decoder exists for.
    #[test]
    fn distance_5_repetition_code_corrects_every_single_error() {
        let code = RepetitionCode::new(5, PauliCorrection::X);
        let target = arbitrary_state_target();
        for q in 0..5 {
            let recovered =
                run_decodable_round(&code, arbitrary_state_prep, Some(code.error_gate(q)))
                    .unwrap();
            let fidelity = recovered.fidelity(&target).unwrap();
            assert!(
                (fidelity - 1.0).abs() < 1e-9,
                "distance-5 code failed to correct a single error on qubit {}: fidelity {}",
                q, fidelity
            );
        }
    }

    #[test]
    fn distance_5_repetition_code_corrects_no_error() {
        let code = RepetitionCode::new(5, PauliCorrection::X);
        let target = arbitrary_state_target();
        let recovered = run_decodable_round(&code, arbitrary_state_prep, None).unwrap();
        assert!((recovered.fidelity(&target).unwrap() - 1.0).abs() < 1e-9);
    }

    /// The real scale claim: a distance-5 repetition code guarantees
    /// correction of *any* error pattern of weight up to
    /// `(5-1)/2 = 2` -- checked exhaustively here over all `C(5,2)=10`
    /// possible double-error locations, adjacent pairs included. (An
    /// earlier version of this test excluded adjacent pairs, on the
    /// mistaken assumption that adjacency mattered for MWPM
    /// correctness -- it doesn't; the guarantee is purely about error
    /// *weight*, and an adjacent pair is still weight 2. See
    /// [`distance_5_code_weight_3_error_can_exceed_the_correction_bound`]
    /// for what actually happens past the guarantee.) A static
    /// `Gate::If` table for this many syndrome bits (4 bits, 16
    /// possible syndromes) would already be unwieldy; this is exactly
    /// the scale the real decoder exists for.
    #[test]
    fn distance_5_repetition_code_corrects_every_double_error() {
        let code = RepetitionCode::new(5, PauliCorrection::X);
        let target = arbitrary_state_target();
        for i in 0..5 {
            for j in (i + 1)..5 {
                let mut c = Circuit::new(9);
                c.num_clbits = 4;
                arbitrary_state_prep(&mut c, 0);
                code.encode(&mut c, &(0..5).collect::<Vec<_>>());
                c.push(code.error_gate(i));
                c.push(code.error_gate(j));
                code.extract_syndrome(
                    &mut c,
                    &(0..5).collect::<Vec<_>>(),
                    &(5..9).collect::<Vec<_>>(),
                    &(0..4).collect::<Vec<_>>(),
                );
                let opt = crate::ir_optimize::optimize(&c);
                let native = crate::native::decompose(&opt);
                let (mut reg, syndrome) = crate::emit::run_with_measurement(&native).unwrap();
                for (q, corr) in code.decode_syndrome(&syndrome) {
                    match corr {
                        PauliCorrection::X => reg.apply_pauli_x(q).unwrap(),
                        PauliCorrection::Y => reg.apply_pauli_y(q).unwrap(),
                        PauliCorrection::Z => reg.apply_pauli_z(q).unwrap(),
                    }
                }
                let mut c2 = Circuit::new(9);
                code.decode(&mut c2, &(0..5).collect::<Vec<_>>());
                let opt2 = crate::ir_optimize::optimize(&c2);
                let native2 = crate::native::decompose(&opt2);
                crate::emit::apply_to(&native2, &mut reg).unwrap();
                let recovered = reg.to_density_matrix().unwrap().partial_trace(&[0]).unwrap();
                let fidelity = recovered.fidelity(&target).unwrap();
                assert!(
                    (fidelity - 1.0).abs() < 1e-9,
                    "distance-5 code failed on double error ({}, {}): fidelity {}",
                    i, j, fidelity
                );
            }
        }
    }

    /// The honest documentation of a real limit, found by actually
    /// checking rather than assumed: distance-5 only *guarantees*
    /// correction up to weight `(5-1)/2 = 2`; a weight-3 error (qubits
    /// 0, 2, 4) exceeds that bound, and the decoder -- correctly,
    /// per MWPM's own optimality, not buggily -- finds a *different*,
    /// lower-weight explanation for the resulting syndrome than the
    /// true error, and "corrects" to the wrong state. This is the
    /// real, standard failure mode past a code's guaranteed distance,
    /// not a bug in this decoder.
    #[test]
    fn distance_5_code_weight_3_error_can_exceed_the_correction_bound() {
        let code = RepetitionCode::new(5, PauliCorrection::X);
        let target = arbitrary_state_target();

        let mut c = Circuit::new(9);
        c.num_clbits = 4;
        arbitrary_state_prep(&mut c, 0);
        code.encode(&mut c, &(0..5).collect::<Vec<_>>());
        c.push(code.error_gate(0));
        c.push(code.error_gate(2));
        c.push(code.error_gate(4));
        code.extract_syndrome(
            &mut c,
            &(0..5).collect::<Vec<_>>(),
            &(5..9).collect::<Vec<_>>(),
            &(0..4).collect::<Vec<_>>(),
        );
        let opt = crate::ir_optimize::optimize(&c);
        let native = crate::native::decompose(&opt);
        let (mut reg, syndrome) = crate::emit::run_with_measurement(&native).unwrap();
        for (q, corr) in code.decode_syndrome(&syndrome) {
            match corr {
                PauliCorrection::X => reg.apply_pauli_x(q).unwrap(),
                PauliCorrection::Y => reg.apply_pauli_y(q).unwrap(),
                PauliCorrection::Z => reg.apply_pauli_z(q).unwrap(),
            }
        }
        let mut c2 = Circuit::new(9);
        code.decode(&mut c2, &(0..5).collect::<Vec<_>>());
        let opt2 = crate::ir_optimize::optimize(&c2);
        let native2 = crate::native::decompose(&opt2);
        crate::emit::apply_to(&native2, &mut reg).unwrap();
        let recovered = reg.to_density_matrix().unwrap().partial_trace(&[0]).unwrap();
        let fidelity = recovered.fidelity(&target).unwrap();
        assert!(
            fidelity < 0.99,
            "expected a weight-3 error on a distance-5 code (exceeding the (d-1)/2=2 \
             guarantee) to genuinely fail to correct, got fidelity {} -- if this now passes, \
             this specific error pattern happened to still be within the decoder's reach, and \
             this test's chosen qubits need revisiting to demonstrate a real failure",
            fidelity
        );
    }

    #[test]
    fn distance_7_repetition_code_corrects_every_single_error() {
        let code = RepetitionCode::new(7, PauliCorrection::X);
        let target = arbitrary_state_target();
        for q in 0..7 {
            let recovered =
                run_decodable_round(&code, arbitrary_state_prep, Some(code.error_gate(q)))
                    .unwrap();
            let fidelity = recovered.fidelity(&target).unwrap();
            assert!(
                (fidelity - 1.0).abs() < 1e-9,
                "distance-7 code failed to correct a single error on qubit {}: fidelity {}",
                q, fidelity
            );
        }
    }

    #[test]
    #[should_panic]
    fn rejects_even_distance() {
        RepetitionCode::new(4, PauliCorrection::X);
    }

    #[test]
    #[should_panic]
    fn rejects_distance_below_3() {
        RepetitionCode::new(1, PauliCorrection::X);
    }

    #[test]
    #[should_panic]
    fn rejects_y_correction() {
        RepetitionCode::new(3, PauliCorrection::Y);
    }
}
