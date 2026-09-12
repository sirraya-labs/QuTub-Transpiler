//! The open extension point for adding a quantum error-correcting code
//! to this crate -- deliberately mirrors [`crate::backend::BackendSpec`]'s
//! own one-trait-per-implementation pattern (see that module's doc
//! comment for the rationale, which applies here unchanged): implement
//! [`StabilizerCode`] once, in its own file under `src/qec/`, to add a
//! new code. [`bit_flip::ThreeQubitBitFlipCode`] and
//! [`phase_flip::ThreeQubitPhaseFlipCode`] are the two codes shipped
//! today.
//!
//! # Why these two codes, and not more, today
//! Both are the standard three-qubit repetition codes from the
//! original QEC literature -- Shor, "Scheme for reducing decoherence
//! in quantum computer memory," *Phys. Rev. A* 52, R2493 (1995), and
//! the modern textbook treatment in Nielsen & Chuang, *Quantum
//! Computation and Quantum Information*, Section 10.1 ("Three qubit
//! bit flip code") and 10.1.2 ("Three qubit phase flip code"). Their
//! syndrome decoding is exactly a lookup table over two classical
//! bits, which is precisely what [`crate::ir::Gate::If`]'s multi-bit
//! AND conditions exist to express -- see [`StabilizerCode::correct`]'s
//! own doc comment on each code's file for the exact table.
//!
//! What's deliberately *not* here yet: any code whose decoder isn't a
//! small, explicit lookup table. A real surface code (or any code at
//! meaningful distance) needs a genuine external decoding algorithm --
//! minimum-weight perfect matching, union-find, or a trained neural
//! decoder -- that runs real classical computation on the syndrome,
//! not a static branch table `Gate::If`/`SeqInstr::JumpIfEqual` can
//! express. Building a fake version of that (a lookup table dressed up
//! to look like a real decoder) would be exactly the kind of
//! fabricated-to-look-complete work this crate's conventions elsewhere
//! refuse to produce -- see `sequencer.rs`'s own doc comment on why no
//! `HardwareTarget` implementation ships without a real vendor spec to
//! check it against, for the same reasoning applied here. A real
//! surface-code (or similar) implementation is genuine, larger,
//! separate follow-on work: it needs `StabilizerCode` (or a
//! generalization of it) *plus* a real decoding-algorithm integration
//! this crate doesn't have yet.
//!
//! # Verification methodology
//! Every claim this module's own tests make ("this code corrects any
//! single bit-flip error") is checked against the real simulator, the
//! same standard every other rewrite in this crate is held to (see
//! `tests/decompositions.rs`, `examples/verify_equivalence.rs`,
//! `sequencer.rs`'s own tests): prepare an arbitrary logical state,
//! inject a specific error, run the *real* encode -> syndrome-extract
//! -> `Gate::If`-conditioned correct -> decode circuit, and compare
//! the recovered qubit's density matrix against the original
//! preparation's, via `DensityMatrix::fidelity` -- not asserted
//! algebraically.

use crate::ir::Circuit;

pub mod bit_flip;
pub mod decoder;
pub mod phase_flip;
pub mod repetition;

/// One quantum error-correcting code: how to encode a single logical
/// qubit into several physical ones, how to extract a syndrome via
/// ancilla qubits, and how to classically decode that syndrome into a
/// correction -- see this module's doc comment for the two codes
/// shipped today and what it would take to add another.
///
/// Every method appends gates to an existing `Circuit` (the same
/// "build onto a caller-supplied circuit" shape
/// [`crate::backend::BackendSpec::push_two_qubit_zz`] already uses)
/// rather than returning a new one, so a caller can freely interleave
/// a code's own gates with anything else in a larger circuit (state
/// preparation, a deliberately-injected test error, multiple rounds).
pub trait StabilizerCode: Send + Sync {
    /// Stable identifier, e.g. `"ThreeQubitBitFlip"`. Same role as
    /// [`crate::backend::BackendSpec::id`].
    fn id(&self) -> &'static str;

    /// How many physical data qubits this code uses to encode one
    /// logical qubit. Every method below that takes a `data_qubits`
    /// slice requires it to have exactly this length.
    fn num_data_qubits(&self) -> usize;

    /// How many stabilizer generators this code measures -- equal to
    /// the number of ancilla qubits [`extract_syndrome`](Self::extract_syndrome)
    /// needs and the number of classical bits the syndrome occupies.
    fn num_syndrome_bits(&self) -> usize;

    /// Appends the encoding circuit: `data_qubits[0]` holds the
    /// logical qubit's state to encode (any single-qubit state a
    /// caller already prepared there); every other data qubit must
    /// start in `|0>`. After this, the logical state is spread across
    /// all of `data_qubits`.
    fn encode(&self, circuit: &mut Circuit, data_qubits: &[usize]);

    /// Appends syndrome extraction: measures every stabilizer
    /// generator via `ancilla_qubits` (which must start in `|0>`,
    /// length [`num_syndrome_bits`](Self::num_syndrome_bits)), writing
    /// each result into the corresponding entry of `syndrome_clbits`
    /// (same length). Does not modify `data_qubits`' encoded state --
    /// a stabilizer measurement's whole point is projecting onto an
    /// eigenspace without disturbing the encoded logical information.
    fn extract_syndrome(
        &self,
        circuit: &mut Circuit,
        data_qubits: &[usize],
        ancilla_qubits: &[usize],
        syndrome_clbits: &[usize],
    );

    /// Appends the classically-conditioned correction -- built
    /// entirely from [`crate::ir::Gate::If`] reading `syndrome_clbits`,
    /// the real thing this method exists to demonstrate (see this
    /// module's own doc comment). See each concrete code's own doc
    /// comment for its exact syndrome-to-correction table.
    fn correct(&self, circuit: &mut Circuit, data_qubits: &[usize], syndrome_clbits: &[usize]);

    /// Appends the decoding circuit that reverses
    /// [`encode`](Self::encode), collapsing the logical information
    /// back onto `data_qubits[0]` alone. Meant for verification (and
    /// for a caller that wants the logical qubit back for further use)
    /// -- assumes [`correct`](Self::correct) already fixed any error,
    /// the same precondition real error-correction always has.
    fn decode(&self, circuit: &mut Circuit, data_qubits: &[usize]);
}

/// Which single-qubit Pauli a real decoder ([`decoder`]-based code)
/// applies as a correction -- separate from
/// [`crate::noise::PauliError`] (that one names a sampled *error*;
/// this one names a computed *correction* -- same three values,
/// different role, kept as distinct types so a caller can't
/// accidentally pass one where the other was meant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauliCorrection {
    X,
    Y,
    Z,
}

/// A stabilizer code whose syndrome is decoded by a real classical
/// algorithm ([`decoder::minimum_weight_perfect_matching`]), not a
/// static [`crate::ir::Gate::If`] lookup table -- the general case for
/// any code with more syndrome bits than a static branch table could
/// reasonably enumerate (`2^(num_syndrome_bits)` entries), which is
/// to say: real QEC at any meaningful scale. See `qec/mod.rs`'s own
/// doc comment for why this genuinely needs to be a second trait
/// rather than an extension of [`StabilizerCode`] -- the correction
/// depends on *runtime-computed* data, which has no
/// [`crate::ir::Gate::If`]-expressible representation to hand back
/// from a method like [`StabilizerCode::correct`] does.
///
/// [`run_decodable_round`] is the real execution shape this implies:
/// run the syndrome-extraction circuit for real, read back the real
/// classical bits, decode them in real software, then apply the
/// computed correction directly to the (already-executing) register --
/// two genuine stages, not one static circuit, because the second
/// stage's content isn't knowable until the first stage's real
/// measurement outcomes exist.
pub trait DecodableCode: Send + Sync {
    fn id(&self) -> &'static str;
    fn num_data_qubits(&self) -> usize;
    fn num_syndrome_bits(&self) -> usize;
    fn encode(&self, circuit: &mut Circuit, data_qubits: &[usize]);
    fn extract_syndrome(
        &self,
        circuit: &mut Circuit,
        data_qubits: &[usize],
        ancilla_qubits: &[usize],
        syndrome_clbits: &[usize],
    );
    fn decode(&self, circuit: &mut Circuit, data_qubits: &[usize]);

    /// Given the real classical bits [`extract_syndrome`](Self::extract_syndrome)
    /// wrote (indexed the same way, `syndrome[i]` for
    /// `syndrome_clbits[i]`), computes which physical data qubits need
    /// which correction -- real classical computation, not a static
    /// circuit. `(qubit, PauliCorrection::X)` means "apply `X` to this
    /// data qubit," etc.
    fn decode_syndrome(&self, syndrome: &[u8]) -> Vec<(usize, PauliCorrection)>;
}

/// Runs one full round for any [`DecodableCode`]: prepare an arbitrary
/// state via `prep`, encode, optionally inject `injected_error`,
/// extract syndrome for real, decode the real syndrome bits in real
/// software, apply the computed correction directly to the executing
/// register, then run the code's own `decode` circuit to collapse the
/// logical information back onto the first data qubit -- returning its
/// reduced density matrix. See [`DecodableCode`]'s own doc comment for
/// why this needs two real execution stages rather than one static
/// circuit, and [`qec::tests::run_one_round`](tests) for the
/// [`StabilizerCode`] (static-table) sibling this mirrors.
pub fn run_decodable_round(
    code: &dyn DecodableCode,
    prep: impl Fn(&mut Circuit, usize),
    injected_error: Option<crate::ir::Gate>,
) -> Result<sirraya_qutub::DensityMatrix, String> {
    let n = code.num_data_qubits();
    let s = code.num_syndrome_bits();
    let data_qubits: Vec<usize> = (0..n).collect();
    let ancilla_qubits: Vec<usize> = (n..n + s).collect();
    let syndrome_clbits: Vec<usize> = (0..s).collect();

    // Stage 1: prep, encode, (test) error, extract syndrome -- run for
    // real to get real classical bits.
    let mut c1 = Circuit::new(n + s);
    c1.num_clbits = s;
    prep(&mut c1, data_qubits[0]);
    code.encode(&mut c1, &data_qubits);
    if let Some(g) = injected_error {
        c1.push(g);
    }
    code.extract_syndrome(&mut c1, &data_qubits, &ancilla_qubits, &syndrome_clbits);
    let optimized1 = crate::ir_optimize::optimize(&c1);
    let native1 = crate::native::decompose(&optimized1);
    let (mut reg, syndrome) = crate::emit::run_with_measurement(&native1)?;

    // Real classical decoding, on the real syndrome just measured.
    let corrections = code.decode_syndrome(&syndrome);

    // Stage 2: apply the computed correction directly to the
    // already-executing register, then run the code's own decode
    // circuit on top of the same register.
    for (q, correction) in corrections {
        match correction {
            PauliCorrection::X => reg.apply_pauli_x(q)?,
            PauliCorrection::Y => reg.apply_pauli_y(q)?,
            PauliCorrection::Z => reg.apply_pauli_z(q)?,
        }
    }
    let mut c2 = Circuit::new(n + s);
    code.decode(&mut c2, &data_qubits);
    let optimized2 = crate::ir_optimize::optimize(&c2);
    let native2 = crate::native::decompose(&optimized2);
    crate::emit::apply_to(&native2, &mut reg)?;

    reg.to_density_matrix()?.partial_trace(&[data_qubits[0]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit;
    use crate::ir::Gate;
    use crate::qec::bit_flip::ThreeQubitBitFlipCode;
    use crate::qec::phase_flip::ThreeQubitPhaseFlipCode;
    use sirraya_qutub::core::QuantumRegister;
    use sirraya_qutub::DensityMatrix;

    /// Builds and runs one full QEC round for any [`StabilizerCode`]:
    /// prepare an arbitrary state on the logical qubit via `prep`,
    /// encode, optionally inject `injected_error` (a single gate,
    /// standing in for a physical error on one qubit at one point in
    /// time), extract syndrome, classically-conditioned correct,
    /// decode -- then return the recovered qubit's reduced density
    /// matrix. Generic over `code`, so the exact same harness verifies
    /// every concrete `StabilizerCode` this crate ships, which is the
    /// real payoff of the trait being genuinely modular (see
    /// `qec/mod.rs`'s own doc comment).
    fn run_one_round(
        code: &dyn StabilizerCode,
        prep: impl Fn(&mut Circuit, usize),
        injected_error: Option<Gate>,
    ) -> DensityMatrix {
        let n = code.num_data_qubits();
        let s = code.num_syndrome_bits();
        let mut c = Circuit::new(n + s);
        c.num_clbits = s;

        let data_qubits: Vec<usize> = (0..n).collect();
        let ancilla_qubits: Vec<usize> = (n..n + s).collect();
        let syndrome_clbits: Vec<usize> = (0..s).collect();

        prep(&mut c, data_qubits[0]);
        code.encode(&mut c, &data_qubits);
        if let Some(g) = injected_error {
            c.push(g);
        }
        code.extract_syndrome(&mut c, &data_qubits, &ancilla_qubits, &syndrome_clbits);
        code.correct(&mut c, &data_qubits, &syndrome_clbits);
        code.decode(&mut c, &data_qubits);

        let optimized = crate::ir_optimize::optimize(&c);
        let native = crate::native::decompose(&optimized);
        let (reg, _clbits) = emit::run_with_measurement(&native).unwrap();
        reg.to_density_matrix()
            .unwrap()
            .partial_trace(&[data_qubits[0]])
            .unwrap()
    }

    fn plus_state_target() -> DensityMatrix {
        let mut reg = QuantumRegister::new(1).unwrap();
        reg.apply_hadamard(0).unwrap();
        reg.to_density_matrix().unwrap()
    }

    fn arbitrary_state_prep(c: &mut Circuit, q: usize) {
        // Ry(0.7) then Rz(1.3) on |0> -- a generic, non-basis-aligned
        // state, the same style of coverage
        // `examples/quantum_teleportation.rs` already uses for its own
        // "arbitrary state" case.
        c.push(Gate::Rz(q, 1.3));
        c.push(Gate::Ry(q, 0.7));
    }

    fn arbitrary_state_target() -> DensityMatrix {
        let mut reg = QuantumRegister::new(1).unwrap();
        reg.apply_rz(0, 1.3).unwrap();
        reg.apply_ry(0, 0.7).unwrap();
        reg.to_density_matrix().unwrap()
    }

    #[test]
    fn bit_flip_code_corrects_any_single_x_error() {
        let code = ThreeQubitBitFlipCode;
        for (prep, target) in [
            (
                arbitrary_state_prep as fn(&mut Circuit, usize),
                arbitrary_state_target(),
            ),
            (
                |c: &mut Circuit, q: usize| {
                    c.push(Gate::H(q));
                },
                plus_state_target(),
            ),
        ] {
            for injected in [None, Some(Gate::X(0)), Some(Gate::X(1)), Some(Gate::X(2))] {
                let recovered = run_one_round(&code, prep, injected.clone());
                let fidelity = recovered.fidelity(&target).unwrap();
                assert!(
                    (fidelity - 1.0).abs() < 1e-9,
                    "bit-flip code, injected error {:?}: expected ~100% recovery fidelity, got {}",
                    injected,
                    fidelity
                );
            }
        }
    }

    #[test]
    fn phase_flip_code_corrects_any_single_z_error() {
        let code = ThreeQubitPhaseFlipCode;
        for (prep, target) in [
            (
                arbitrary_state_prep as fn(&mut Circuit, usize),
                arbitrary_state_target(),
            ),
            (
                |c: &mut Circuit, q: usize| {
                    c.push(Gate::H(q));
                },
                plus_state_target(),
            ),
        ] {
            for injected in [None, Some(Gate::Z(0)), Some(Gate::Z(1)), Some(Gate::Z(2))] {
                let recovered = run_one_round(&code, prep, injected.clone());
                let fidelity = recovered.fidelity(&target).unwrap();
                assert!(
                    (fidelity - 1.0).abs() < 1e-9,
                    "phase-flip code, injected error {:?}: expected ~100% recovery fidelity, got {}",
                    injected, fidelity
                );
            }
        }
    }

    /// The honest limit, checked *precisely* rather than just claimed
    /// in prose (see `qec/mod.rs`'s "what this does not correct" doc
    /// comments): the bit-flip code's stabilizers are `Z`-type, which
    /// commute with any `Z` error regardless of location, so the
    /// syndrome always reads `(0,0)` and no correction ever fires. A
    /// single physical `Z` error is, for this code, mathematically
    /// identical to its own logical `Z_L` operator (`Zi|000>=|000>`,
    /// `Zi|111>=-|111>` -- exactly the action a logical `Z` should
    /// have, for any single `i`) -- so the recovered state should
    /// match *exactly* what a bare `Z` on the original preparation
    /// gives, computed independently here, not merely "some
    /// corruption happened below a threshold."
    #[test]
    fn bit_flip_code_does_not_correct_a_z_error_it_is_the_logical_z() {
        let code = ThreeQubitBitFlipCode;
        let target = arbitrary_state_target();
        let recovered = run_one_round(&code, arbitrary_state_prep, Some(Gate::Z(1)));

        let expected_logical_z: DensityMatrix = {
            let mut reg = QuantumRegister::new(1).unwrap();
            reg.apply_rz(0, 1.3).unwrap();
            reg.apply_ry(0, 0.7).unwrap();
            reg.apply_pauli_z(0).unwrap();
            reg.to_density_matrix().unwrap()
        };
        let expected_fidelity_vs_original = expected_logical_z.fidelity(&target).unwrap();
        assert!(
            expected_fidelity_vs_original < 0.99,
            "test setup: the chosen state's logical-Z fidelity against itself should not be \
             ~1.0, got {} -- pick a different arbitrary_state_prep angle",
            expected_fidelity_vs_original
        );

        let actual_fidelity_vs_logical_z = recovered.fidelity(&expected_logical_z).unwrap();
        assert!(
            (actual_fidelity_vs_logical_z - 1.0).abs() < 1e-9,
            "expected a single Z error on this code to act as exactly the logical Z operator \
             (fidelity {} against the predicted Z-flipped state, expected ~1.0) -- got fidelity \
             {} against the original, un-Z'd target instead",
            actual_fidelity_vs_logical_z,
            recovered.fidelity(&target).unwrap()
        );
    }

    /// The symmetric honest limit for the phase-flip code, stated
    /// precisely: a *single* physical `X` error isn't merely
    /// "uncorrected corruption" for this code -- it's mathematically
    /// identical to the code's own **logical `Z` operator**. The
    /// phase-flip code is the bit-flip code conjugated by `H` on every
    /// data qubit (see `qec/phase_flip.rs`'s own doc comment), and the
    /// bit-flip code's logical `Z_L` is any single physical `Zi`
    /// (`Zi|000> = |000>`, `Zi|111> = -|111>` -- exactly a logical `Z`
    /// action); conjugating by `H` turns that single `Zi` into a
    /// single `Xi` (`H.Z.H = X`). So a lone physical `X` error, for
    /// this code specifically, is indistinguishable from a *deliberate*
    /// logical `Z` gate -- not detected, not corrected, because the
    /// code's `X`-type stabilizers were never going to catch it (an
    /// earlier version of this test wrongly expected the result to
    /// match a *bare X* on the original state; the correct prediction,
    /// confirmed below, is a bare *Z*).
    #[test]
    fn phase_flip_code_does_not_correct_an_x_error_it_is_the_logical_z() {
        let code = ThreeQubitPhaseFlipCode;
        let target = arbitrary_state_target();
        let recovered = run_one_round(&code, arbitrary_state_prep, Some(Gate::X(1)));

        let expected_logical_z: DensityMatrix = {
            let mut reg = QuantumRegister::new(1).unwrap();
            reg.apply_rz(0, 1.3).unwrap();
            reg.apply_ry(0, 0.7).unwrap();
            reg.apply_pauli_z(0).unwrap();
            reg.to_density_matrix().unwrap()
        };
        let expected_fidelity_vs_original = expected_logical_z.fidelity(&target).unwrap();
        assert!(
            expected_fidelity_vs_original < 0.99,
            "test setup: the chosen state's logical-Z fidelity against itself should not be \
             ~1.0, got {} -- pick a different arbitrary_state_prep angle",
            expected_fidelity_vs_original
        );

        let actual_fidelity_vs_logical_z = recovered.fidelity(&expected_logical_z).unwrap();
        assert!(
            (actual_fidelity_vs_logical_z - 1.0).abs() < 1e-9,
            "expected a single X error on this code to act as exactly the logical Z operator \
             (fidelity {} against the predicted Z-flipped state, expected ~1.0) -- got fidelity \
             {} against the original, un-Z'd target instead",
            actual_fidelity_vs_logical_z,
            recovered.fidelity(&target).unwrap()
        );
    }

    /// The other honest limit: distance 3 means *one* error, not two.
    /// Two simultaneous `X` errors on `q0` and `q1` produce a syndrome
    /// indistinguishable from a *different* single error -- `X(q0)`
    /// flips only the first stabilizer, `X(q1)` flips both, so their
    /// XOR is `(0,1)`, exactly the syndrome a lone `X(q2)` would give.
    /// The decoder applies `X(q2)` on top of the two real errors,
    /// leaving a net `X⊗X⊗X` on the three data qubits -- a coherent
    /// *logical* `X` flip, not incoherent garbage. Checked precisely:
    /// the recovered state should match exactly what a bare logical
    /// `X` on the original preparation gives (computed independently),
    /// not merely "some low fidelity against the original." An earlier
    /// version of this test used `|+>` as the prepared state and found
    /// fidelity ~1.0 -- not a bug, but a genuinely blind test vector:
    /// `X|+> = |+>`, so a logical-X-type miscorrection is invisible to
    /// it specifically. `arbitrary_state_prep` isn't an `X` eigenstate,
    /// so it actually reveals the effect.
    #[test]
    fn bit_flip_code_miscorrects_two_simultaneous_errors_as_a_logical_x_flip() {
        let code = ThreeQubitBitFlipCode;
        let target = arbitrary_state_target();

        let mut c = Circuit::new(5);
        c.num_clbits = 2;
        arbitrary_state_prep(&mut c, 0);
        code.encode(&mut c, &[0, 1, 2]);
        c.push(Gate::X(0));
        c.push(Gate::X(1));
        code.extract_syndrome(&mut c, &[0, 1, 2], &[3, 4], &[0, 1]);
        code.correct(&mut c, &[0, 1, 2], &[0, 1]);
        code.decode(&mut c, &[0, 1, 2]);

        let optimized = crate::ir_optimize::optimize(&c);
        let native = crate::native::decompose(&optimized);
        let (reg, _clbits) = emit::run_with_measurement(&native).unwrap();
        let recovered = reg
            .to_density_matrix()
            .unwrap()
            .partial_trace(&[0])
            .unwrap();

        let expected_logical_x_flip: DensityMatrix = {
            let mut reg = QuantumRegister::new(1).unwrap();
            reg.apply_rz(0, 1.3).unwrap();
            reg.apply_ry(0, 0.7).unwrap();
            reg.apply_pauli_x(0).unwrap();
            reg.to_density_matrix().unwrap()
        };
        let expected_fidelity_vs_original = expected_logical_x_flip.fidelity(&target).unwrap();
        assert!(
            expected_fidelity_vs_original < 0.99,
            "test setup: the chosen state's logical-X-flip fidelity against itself should not \
             be ~1.0, got {} -- pick a different arbitrary_state_prep angle",
            expected_fidelity_vs_original
        );

        let actual_fidelity_vs_flip = recovered.fidelity(&expected_logical_x_flip).unwrap();
        assert!(
            (actual_fidelity_vs_flip - 1.0).abs() < 1e-9,
            "expected two simultaneous errors to produce exactly a net logical X flip \
             (fidelity {} against the predicted flipped state, expected ~1.0) -- got fidelity \
             {} against the original, unflipped target instead",
            actual_fidelity_vs_flip,
            recovered.fidelity(&target).unwrap()
        );
    }
}
