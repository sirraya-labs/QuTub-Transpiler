//! A self-contained circuit-fidelity estimate.
//!
//! # P0.2 finding (resolved)
//!
//! This module used to avoid `sirraya_qutub::xeb::HardwareCalibration`
//! entirely and re-derive its formula independently, because this crate
//! had only ever seen that type's field layout from `xeb.rs`'s doc
//! comments, never a compiled build. That's now confirmed directly from
//! source: `HardwareCalibration` is
//!
//! ```ignore
//! pub struct HardwareCalibration {
//!     pub name: &'static str,
//!     pub single_qubit_fidelity: f64,
//!     pub two_qubit_fidelity: f64,
//! }
//! ```
//!
//! -- field-for-field identical to [`PublishedCalibration`] below, with
//! the same `fidelity_to_depolarizing_probability` formula and the same
//! published Quantinuum Helios numbers (0.999975 / 0.99921) in its own
//! `quantinuum_helios_2026()` constructor. It stores a single published
//! fidelity figure, not per-qubit or time-varying calibration data, so
//! the divergence this module's doc comment used to warn about was never
//! actually there. **Decision:** replace the independent re-derivation
//! for the one entry the two types share (Quantinuum Helios) with a
//! thin wrapper delegating to the real type, so the two numbers cannot
//! silently drift apart in the future -- see
//! [`PublishedCalibration::quantinuum_helios_2026`] below.
//! `ibm_heron_r2` and `rigetti_ankaa3` have no counterpart in
//! `sirraya_qutub::xeb` (it only models the trapped-ion Quantinuum
//! device) and so still stand on their own citations, unchanged.
//!
//! The estimate itself is the standard first-order approximation used
//! for a quick circuit-level fidelity budget: treat each native gate's
//! error as an independent depolarizing event and multiply survival
//! probabilities. It is not a substitute for actually running XEB
//! (`sirraya_qutub::xeb::run_xeb_demo`) against the real noise model --
//! it's a fast, gate-count-based estimate to sanity-check a compiled
//! circuit before paying for a full noisy simulation.

use crate::native::NativeCircuit;

#[derive(Debug, Clone, Copy)]
pub struct PublishedCalibration {
    pub name: &'static str,
    pub single_qubit_fidelity: f64,
    pub two_qubit_fidelity: f64,
}

/// Confirmed field-for-field identical to `PublishedCalibration` (see
/// this module's doc comment) -- this conversion is the thin wrapper
/// P0.2 called for, not a coincidence of matching field names.
impl From<sirraya_qutub::xeb::HardwareCalibration> for PublishedCalibration {
    fn from(cal: sirraya_qutub::xeb::HardwareCalibration) -> Self {
        Self {
            name: cal.name,
            single_qubit_fidelity: cal.single_qubit_fidelity,
            two_qubit_fidelity: cal.two_qubit_fidelity,
        }
    }
}

impl PublishedCalibration {
    /// Quantinuum Helios (98-qubit trapped-ion), as benchmarked by Sandia
    /// National Laboratories and published in Nature, June 2026.
    ///
    /// Delegates to `sirraya_qutub::xeb::HardwareCalibration::quantinuum_helios_2026`
    /// (confirmed field-for-field identical -- see this module's doc
    /// comment) instead of restating the same two numbers a second time,
    /// so this and `sirraya_qutub`'s own copy cannot silently drift
    /// apart the way an independently-maintained duplicate could.
    pub fn quantinuum_helios_2026() -> Self {
        sirraya_qutub::xeb::HardwareCalibration::quantinuum_helios_2026().into()
    }

    /// p = (1 - F) * d / (d - 1), the standard average-gate-fidelity ->
    /// depolarizing-parameter relation (d = 2^num_qubits).
    pub fn fidelity_to_depolarizing_probability(fidelity: f64, num_qubits: usize) -> f64 {
        let d = (1usize << num_qubits) as f64;
        ((1.0 - fidelity) * d / (d - 1.0)).clamp(0.0, 1.0)
    }

    pub fn single_qubit_error_probability(&self) -> f64 {
        Self::fidelity_to_depolarizing_probability(self.single_qubit_fidelity, 1)
    }

    pub fn two_qubit_error_probability(&self) -> f64 {
        Self::fidelity_to_depolarizing_probability(self.two_qubit_fidelity, 2)
    }
}

/// Multiplies each native gate's per-gate survival probability
/// (1 - error_probability) together across the whole circuit. This is
/// the same independent-depolarizing-events approximation
/// `run_xeb_demo` builds a noise *simulation* out of, but computed
/// directly from gate counts rather than by sampling -- O(gates) instead
/// of O(2^n).
pub fn estimate_circuit_fidelity(circuit: &NativeCircuit, cal: &PublishedCalibration) -> f64 {
    let (single_count, two_count) = circuit.gate_counts();
    let single_survival = 1.0 - cal.single_qubit_error_probability();
    let two_survival = 1.0 - cal.two_qubit_error_probability();
    single_survival.powi(single_count as i32) * two_survival.powi(two_count as i32)
}

/// Same estimate as [`estimate_circuit_fidelity`], but for a
/// backend-lowered [`crate::backend::BackendCircuit`] instead of a
/// [`NativeCircuit`] -- so a circuit lowered to `IbmQ`/`Rigetti` can be
/// budgeted against *that* backend's own published numbers, not
/// against Quantinuum Helios's trapped-ion figures.
pub fn estimate_backend_circuit_fidelity(
    circuit: &crate::backend::BackendCircuit,
    cal: &PublishedCalibration,
) -> f64 {
    let (single_count, two_count) = circuit.gate_counts();
    let single_survival = 1.0 - cal.single_qubit_error_probability();
    let two_survival = 1.0 - cal.two_qubit_error_probability();
    single_survival.powi(single_count as i32) * two_survival.powi(two_count as i32)
}

impl PublishedCalibration {
    /// IBM Heron r2 (156-qubit superconducting, tunable couplers),
    /// IBM's production processor as of this writing. Two-qubit figure
    /// is IBM's own widely-repeated headline number -- a median CZ gate
    /// error rate of ~0.3% (see IBM's processor-types documentation and
    /// multiple third-party benchmark writeups). Single-qubit figure is
    /// from a specific, dated randomized-benchmarking characterization
    /// of an `ibm_boston` Heron r2 device (arXiv, backend snapshot
    /// 2025-12-26): average single-qubit error 1.8e-4, i.e. fidelity
    /// 0.99982. That single-qubit number is for one specific device on
    /// one specific day rather than an IBM-published spec, so treat it
    /// as representative-of-generation rather than an exact guarantee
    /// for any given Heron r2 chip -- the same caveat this module's doc
    /// comment already applies to `HardwareCalibration` in general.
    pub fn ibm_heron_r2() -> Self {
        Self {
            name: "IBM Heron r2 (156-qubit superconducting; 2Q error ~0.3% per IBM; \
                   1Q error 1.8e-4 per arXiv:2603.03496 backend snapshot 2025-12-26)",
            single_qubit_fidelity: 0.99982,
            two_qubit_fidelity: 0.997,
        }
    }

    /// Rigetti Ankaa-3 (84-qubit superconducting, tunable couplers),
    /// launched December 2024. Two-qubit figure is Rigetti's own
    /// published milestone: 99.5% median fSim gate fidelity (their
    /// higher-fidelity native two-qubit gate; the launch announcement
    /// separately reports 99.0% for iSWAP).
    ///
    /// Rigetti has not published an Ankaa-3-specific single-qubit
    /// fidelity figure alongside the two-qubit milestone. Rather than
    /// invent one, this uses the last figure Rigetti *did* publish for
    /// this device family -- a 99.86% median isolated-randomized-
    /// benchmarking single-qubit fidelity reported for the predecessor
    /// Ankaa-2 generation (arXiv:2410.05202). Single-qubit gate fidelity
    /// on fixed-frequency superconducting qubits tends to be stable
    /// across a hardware refresh (unlike the two-qubit entangling gate,
    /// which is what Ankaa-3's redesign specifically targeted), so this
    /// is a reasonable stand-in, but it is a generation *behind* the
    /// two-qubit figure above -- flagged here rather than presented as
    /// equally current.
    pub fn rigetti_ankaa3() -> Self {
        Self {
            name: "Rigetti Ankaa-3 (84-qubit superconducting; 2Q fSim fidelity 99.5% per Rigetti, \
                   Dec 2024; 1Q fidelity 99.86% carried over from Ankaa-2 per arXiv:2410.05202, \
                   not Ankaa-3-specific)",
            single_qubit_fidelity: 0.9986,
            two_qubit_fidelity: 0.995,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantinuum_wrapper_matches_the_real_hardware_calibration_exactly() {
        // Not a fidelity comparison (there's no circuit here) -- a
        // direct field-for-field equality check that the thin wrapper
        // really is transparent, which is the whole point of P0.2's
        // decision to delegate instead of restating the numbers.
        let wrapped = PublishedCalibration::quantinuum_helios_2026();
        let real = sirraya_qutub::xeb::HardwareCalibration::quantinuum_helios_2026();
        assert_eq!(wrapped.name, real.name);
        assert_eq!(wrapped.single_qubit_fidelity, real.single_qubit_fidelity);
        assert_eq!(wrapped.two_qubit_fidelity, real.two_qubit_fidelity);
    }

    #[test]
    fn depolarizing_probability_formula_matches_the_real_crate_for_arbitrary_fidelities() {
        // Same formula, independently invoked on both sides, for a
        // spread of fidelities and qubit counts -- not just the one
        // pinned Quantinuum figure above.
        for &fidelity in &[0.9, 0.99, 0.999975, 0.99921, 1.0, 0.5] {
            for num_qubits in [1usize, 2usize] {
                let ours = PublishedCalibration::fidelity_to_depolarizing_probability(
                    fidelity, num_qubits,
                );
                let theirs = sirraya_qutub::xeb::HardwareCalibration::fidelity_to_depolarizing_probability(
                    fidelity, num_qubits,
                );
                assert!(
                    (ours - theirs).abs() < 1e-15,
                    "fidelity {} num_qubits {}: ours {} vs theirs {}",
                    fidelity, num_qubits, ours, theirs
                );
            }
        }
    }
}
