//! Gate-level depolarizing noise, sampled from this crate's own cited
//! error-rate numbers (`fidelity::PublishedCalibration`) rather than
//! each caller re-deriving or re-guessing its own noise model.
//!
//! # Why this didn't exist as a library module before
//! `fidelity::estimate_circuit_fidelity`/`estimate_backend_circuit_fidelity`
//! already treat each native gate's error as an independent
//! depolarizing event -- but only to multiply survival probabilities
//! into a single aggregate number, never to actually *sample* a noisy
//! trajectory. Every example in this crate that needed a genuinely
//! noisy run (`quantum_teleportation.rs` among them) ended up
//! hand-rolling its own ad hoc Pauli-kick injection at the example
//! level, each with its own slightly different simplification, none
//! of them reusing `PublishedCalibration`'s already-cited single-/
//! two-qubit error rates for the actual sampling. This module is that
//! shared, real implementation: the same numbers `fidelity.rs` already
//! cites, now sampleable, in one place.
//!
//! # The model
//! A standard single-qubit depolarizing channel: with probability `p`
//! (split evenly three ways), apply one of `X`/`Y`/`Z`; with
//! probability `1 - p`, do nothing. For a two-qubit gate, this module
//! applies that same single-qubit channel *independently* to each of
//! the two qubits involved, each at the two-qubit error probability --
//! a standard, simplified stand-in for a full two-qubit depolarizing
//! channel (which would need sampling among 15 nontrivial two-qubit
//! Pauli combinations, not just 3), consistent with the level of
//! simplification `fidelity.rs`'s own aggregate estimate already
//! accepts (see that module's doc comment: "the standard first-order
//! approximation ... not a substitute for actually running XEB").
//!
//! [`crate::sequencer::execute_with_noise`] only samples this for a
//! real physical pulse (`Channel::Drive`/`Channel::Control` `Play`
//! instructions) -- never for a virtual-Z `ShiftPhase`. That's a
//! deliberate, more physically precise choice than
//! `fidelity::estimate_circuit_fidelity`'s own gate-counting (which
//! prices `Rz` the same as `Ry` purely for the simplicity of a quick
//! sanity-check number): a virtual-Z frame update has essentially zero
//! real gate error on real hardware, which is the entire point of
//! implementing it that way rather than as a physical pulse. The two
//! modules disagreeing here is expected, not a bug -- see
//! `sequencer.rs`'s own doc comment on `execute_with_noise` for the
//! explicit note.
//!
//! # Why this crate's library code takes randomness as a parameter
//! Same reasoning as `readout.rs`: this crate's `[dependencies]` has
//! no RNG crate, only `[dev-dependencies]` does. [`sample_depolarizing_error`]
//! is a pure function of an already-drawn `f64 in [0, 1)`, not
//! something that draws its own randomness.

use sirraya_qutub::core::QuantumRegister;

/// Which single-qubit Pauli error a depolarizing-noise sample
/// selected. No `None`/identity variant -- [`sample_depolarizing_error`]
/// returns `Option<PauliError>` instead, so "no error occurred" is
/// `None`, not a fourth variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauliError {
    X,
    Y,
    Z,
}

/// Samples a single-qubit depolarizing-channel outcome: with
/// probability `p` (split evenly three ways: `[0, p/3)` -> `X`,
/// `[p/3, 2p/3)` -> `Y`, `[2p/3, p)` -> `Z`), returns the corresponding
/// error; otherwise (`[p, 1)`) returns `None`. Pure and deterministic
/// given `uniform_sample` -- see this module's doc comment.
pub fn sample_depolarizing_error(p: f64, uniform_sample: f64) -> Option<PauliError> {
    if uniform_sample < p / 3.0 {
        Some(PauliError::X)
    } else if uniform_sample < 2.0 * p / 3.0 {
        Some(PauliError::Y)
    } else if uniform_sample < p {
        Some(PauliError::Z)
    } else {
        None
    }
}

/// Applies `err` to qubit `q` of `reg` -- the real, physical
/// consequence of a [`sample_depolarizing_error`] draw that returned
/// `Some`.
pub fn apply_pauli_error(reg: &mut QuantumRegister, q: usize, err: PauliError) -> Result<(), String> {
    match err {
        PauliError::X => reg.apply_pauli_x(q),
        PauliError::Y => reg.apply_pauli_y(q),
        PauliError::Z => reg.apply_pauli_z(q),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_probability_never_produces_an_error() {
        for &sample in &[0.0, 0.3, 0.5, 0.9999] {
            assert_eq!(sample_depolarizing_error(0.0, sample), None);
        }
    }

    #[test]
    fn certain_probability_always_produces_an_error() {
        for &sample in &[0.0, 0.32, 0.65, 0.9999] {
            assert!(sample_depolarizing_error(1.0, sample).is_some());
        }
    }

    #[test]
    fn the_three_pauli_ranges_are_evenly_split_and_in_order() {
        let p = 0.9;
        // Just inside each third of [0, p).
        assert_eq!(sample_depolarizing_error(p, 0.0), Some(PauliError::X));
        assert_eq!(sample_depolarizing_error(p, p / 3.0 - 1e-9), Some(PauliError::X));
        assert_eq!(sample_depolarizing_error(p, p / 3.0), Some(PauliError::Y));
        assert_eq!(sample_depolarizing_error(p, 2.0 * p / 3.0 - 1e-9), Some(PauliError::Y));
        assert_eq!(sample_depolarizing_error(p, 2.0 * p / 3.0), Some(PauliError::Z));
        assert_eq!(sample_depolarizing_error(p, p - 1e-9), Some(PauliError::Z));
        // Just past p: no error.
        assert_eq!(sample_depolarizing_error(p, p), None);
        assert_eq!(sample_depolarizing_error(p, 0.9999), None);
    }
}