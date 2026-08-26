//! Classical readout (measurement-assignment) error, modeled
//! independently of every other noise source this crate has.
//!
//! # Why this is separate from everything else
//! `fidelity.rs`'s calibrations price gate error. `pulse.rs`'s
//! `PulseCalibration` describes the physical pulse shapes gates and
//! readout compile to. Neither says anything about what happens
//! *after* a qubit is projectively measured: the classical signal
//! chain (amplifier, digitizer, discriminator/decoder) that turns a
//! real analog readout trace into a reported "0" or "1" is itself
//! imperfect, and on real NISQ hardware this is often the *single
//! largest* error source in the whole system -- bigger than any
//! individual gate error (see e.g. Google's own Willow spec sheet,
//! where mean simultaneous repetitive measurement error is larger
//! than either its single- or two-qubit gate error; see this module's
//! own calibrations below for the cited figure).
//!
//! This module models that specific failure mode: a *classical*
//! confusion between the true, exactly-collapsed measurement outcome
//! and what gets reported. It deliberately does not attempt to model
//! *why* the misassignment happens (T1 decay during the integration
//! window vs. a purely electronic discriminator error near the
//! threshold are physically different mechanisms with the same
//! observable signature) -- real vendor calibration data is reported
//! as a single confusion matrix without distinguishing mechanism, and
//! this module matches that level of description rather than
//! inventing a finer-grained physical model no cited source supports.
//!
//! # Where this plugs in
//! [`crate::sequencer::execute_with_readout_noise`] is the actual
//! integration point: it corrupts the classical bit a
//! `SeqInstr::MeasureInto` writes (the value `Gate::If`/
//! `SeqInstr::JumpIfEqual` branch on and the value a caller ultimately
//! reads back), while leaving the *quantum* collapse
//! `QuantumRegister::measure_single_qubit` performs completely exact.
//! That split is deliberate and physically meaningful, not a
//! simplification of convenience: it's the standard way readout error
//! is modeled in the field (e.g. Qiskit's own `ReadoutError` noise
//! channel works the same way -- a purely classical confusion matrix
//! applied on top of an otherwise-ideal simulator).
//!
//! # Why this crate's library code takes randomness as a parameter
//! This crate's `[dependencies]` (Cargo.toml) deliberately has no RNG
//! crate -- only `[dev-dependencies]` does, for examples and tests.
//! [`corrupt_readout`] is therefore a pure function of an already-drawn
//! `f64 in [0, 1)`, not something that draws its own randomness; the
//! caller supplies the sample however it likes (a `rand`-crate RNG in
//! an example, `sirraya_qutub`'s own randomness elsewhere, a fixed
//! sequence in a test). This mirrors how the rest of this crate keeps
//! genuine randomness confined to `sirraya_qutub`'s own measurement
//! collapse rather than adding a second, independent source of it.

use crate::backend::Backend;

/// Confusion probabilities for a single-qubit computational-basis
/// measurement: `p01` is P(report "1" | true state "0"), `p10` is
/// P(report "0" | true state "1"). Real hardware is usually
/// asymmetric -- a qubit truly in `|1>` has a real decay channel
/// (T1) that can flip it to `|0>` during the readout integration
/// window; the reverse process has no comparable mechanism -- so
/// `p10 > p01` is the physically expected direction whenever a cited
/// source actually reports the asymmetric split (see
/// [`ReadoutCalibration::rigetti_ankaa3`] for a real, published
/// example of exactly this asymmetry). Where a cited source only
/// publishes one combined average, both fields are set equal to it --
/// an explicit, stated simplification per calibration, never a claim
/// that the real hardware is actually symmetric.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReadoutCalibration {
    pub name: &'static str,
    pub backend: Backend,
    pub p01: f64,
    pub p10: f64,
}

impl ReadoutCalibration {
    /// The average assignment error, `(p01 + p10) / 2` -- the single
    /// number vendors most often publish in place of the full
    /// confusion matrix (see e.g. Rigetti's own published definition:
    /// "the reported readout fidelity is computed by taking the trace
    /// of the individual confusion matrix and dividing by two," i.e.
    /// exactly this quantity subtracted from 1).
    pub fn average_error(&self) -> f64 {
        (self.p01 + self.p10) / 2.0
    }

    /// Quantinuum Helios (98-qubit trapped-ion). SPAM (state
    /// preparation *and* measurement combined -- Quantinuum's own
    /// published benchmarking methodology reports one number for
    /// both together, not pure measurement-assignment error in
    /// isolation, so this is a mild overestimate of readout error
    /// alone) fidelity ~99.95%, i.e. error rate ~4.8e-4, from
    /// Quantinuum/Sandia's Nature-validated benchmarking of Helios
    /// (launched Nov 2025) -- the same device this crate's
    /// `fidelity::PublishedCalibration::quantinuum_helios_2026`
    /// already cites for gate fidelity. No asymmetric p01/p10 split
    /// has been published for this figure.
    pub fn quantinuum_helios_2026() -> Self {
        Self {
            name: "Quantinuum Helios (98-qubit trapped-ion; SPAM fidelity ~99.95%, error \
                   ~4.8e-4, from Quantinuum/Sandia Nature-validated benchmarking, Nov 2025 \
                   launch -- combined state-prep+measurement, not pure readout in isolation, \
                   and no published asymmetric p01/p10 split)",
            backend: Backend::TrappedIon,
            p01: 4.8e-4,
            p10: 4.8e-4,
        }
    }

    /// IBM Heron r2 (156-qubit superconducting), specifically
    /// `ibm_marrakesh`. Device-wide average readout-error rate 3%,
    /// from a Nov-Dec 2024 calibration snapshot (arXiv:2510.13577,
    /// Table S3) -- a different, earlier snapshot than this crate's
    /// own `fidelity::PublishedCalibration::ibm_heron_r2`'s single-/
    /// two-qubit *gate* error citation (Dec 2025), used here because
    /// that source doesn't itself report readout error. No asymmetric
    /// p01/p10 split published for this figure (a device- and
    /// direction-averaged number).
    pub fn ibm_heron_r2() -> Self {
        Self {
            name: "IBM Heron r2 (156-qubit superconducting, ibm_marrakesh; device-wide \
                   average readout-error rate 3%, Nov-Dec 2024 snapshot, per arXiv:2510.13577 \
                   Table S3 -- no published asymmetric p01/p10 split)",
            backend: Backend::IbmQ,
            p01: 0.03,
            p10: 0.03,
        }
    }

    /// Rigetti Ankaa-3 (84-qubit superconducting). A genuine,
    /// published *asymmetric* confusion matrix -- not a combined
    /// average this module had to symmetrize itself:
    /// P(report 1 | true 0) = 2.40%, P(report 0 | true 1) = 7.26%.
    /// The ~3x asymmetry is exactly the physically expected direction
    /// (see this struct's own doc comment on T1 decay during
    /// readout), and having a real cited source for it rather than
    /// assuming it is why this is the one calibration in this module
    /// with `p01 != p10`. Per arXiv:2604.19832, Eq. (10).
    pub fn rigetti_ankaa3() -> Self {
        Self {
            name: "Rigetti Ankaa-3 (84-qubit superconducting); published confusion matrix \
                   P(1|0)=2.40%, P(0|1)=7.26%, per arXiv:2604.19832 Eq. (10)",
            backend: Backend::Rigetti,
            p01: 0.0240,
            p10: 0.0726,
        }
    }

    /// Google Willow Chip 1 (105-qubit superconducting, CZ-tuned
    /// configuration, Dec 2024). Mean simultaneous repetitive
    /// measurement error 0.77%, from the same official Willow spec
    /// sheet this crate's own
    /// `fidelity::PublishedCalibration::google_willow_2024` already
    /// cites for its gate-fidelity figures (see that constructor's own
    /// doc comment for the full citation and why Chip 1 specifically)
    /// -- kept to the same primary source deliberately, rather than
    /// pulling a readout figure from a different characterization. No
    /// asymmetric p01/p10 split published for this figure.
    pub fn google_willow_2024() -> Self {
        Self {
            name: "Google Willow Chip 1 (105-qubit superconducting, CZ-tuned; mean \
                   simultaneous repetitive measurement error 0.77%, per Google's own Willow \
                   spec sheet, published Dec 9 2024 -- no published asymmetric p01/p10 split)",
            backend: Backend::Google,
            p01: 0.0077,
            p10: 0.0077,
        }
    }
}

/// Applies `cal`'s confusion probabilities to a `true_bit` (the real,
/// exact outcome of a projective measurement -- e.g. from
/// `QuantumRegister::measure_single_qubit`), returning the classical
/// bit a real, imperfect readout chain would actually report. Pure
/// and deterministic given `uniform_sample` (drawn from U[0, 1) by
/// the caller) -- see this module's doc comment on why library code
/// here never draws its own randomness.
pub fn corrupt_readout(true_bit: u8, cal: &ReadoutCalibration, uniform_sample: f64) -> u8 {
    let flip_probability = if true_bit == 0 { cal.p01 } else { cal.p10 };
    if uniform_sample < flip_probability {
        1 - true_bit
    } else {
        true_bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_published_calibration_has_valid_probabilities() {
        for cal in [
            ReadoutCalibration::quantinuum_helios_2026(),
            ReadoutCalibration::ibm_heron_r2(),
            ReadoutCalibration::rigetti_ankaa3(),
            ReadoutCalibration::google_willow_2024(),
        ] {
            assert!((0.0..=1.0).contains(&cal.p01), "{}: p01 out of range", cal.name);
            assert!((0.0..=1.0).contains(&cal.p10), "{}: p10 out of range", cal.name);
        }
    }

    #[test]
    fn rigetti_calibration_is_genuinely_asymmetric() {
        // The one calibration here with a real published p01 != p10 --
        // confirms this module isn't quietly symmetrizing everything.
        let cal = ReadoutCalibration::rigetti_ankaa3();
        assert!(cal.p10 > cal.p01, "expected the physically-typical p10 > p01 asymmetry");
    }

    #[test]
    fn corrupt_readout_respects_the_strict_less_than_threshold() {
        let cal = ReadoutCalibration {
            name: "test",
            backend: Backend::TrappedIon,
            p01: 0.1,
            p10: 0.2,
        };
        // true=0: flips iff sample < 0.1
        assert_eq!(corrupt_readout(0, &cal, 0.05), 1);
        assert_eq!(corrupt_readout(0, &cal, 0.1), 0); // exactly at threshold: not flipped
        assert_eq!(corrupt_readout(0, &cal, 0.5), 0);
        // true=1: flips iff sample < 0.2
        assert_eq!(corrupt_readout(1, &cal, 0.1), 0);
        assert_eq!(corrupt_readout(1, &cal, 0.2), 1);
        assert_eq!(corrupt_readout(1, &cal, 0.5), 1);
    }

    #[test]
    fn zero_error_calibration_never_flips() {
        let cal = ReadoutCalibration { name: "test", backend: Backend::TrappedIon, p01: 0.0, p10: 0.0 };
        for &sample in &[0.0, 0.3, 0.9999] {
            assert_eq!(corrupt_readout(0, &cal, sample), 0);
            assert_eq!(corrupt_readout(1, &cal, sample), 1);
        }
    }

    #[test]
    fn certain_error_calibration_always_flips() {
        let cal = ReadoutCalibration { name: "test", backend: Backend::TrappedIon, p01: 1.0, p10: 1.0 };
        for &sample in &[0.0, 0.3, 0.9999] {
            assert_eq!(corrupt_readout(0, &cal, sample), 1);
            assert_eq!(corrupt_readout(1, &cal, sample), 0);
        }
    }

    #[test]
    fn average_error_matches_the_rigetti_style_definition() {
        let cal = ReadoutCalibration::rigetti_ankaa3();
        assert!((cal.average_error() - (0.0240 + 0.0726) / 2.0).abs() < 1e-12);
    }
}