//! A self-contained circuit-fidelity estimate.
//!
//! This deliberately does **not** import `sirraya_qutub`'s
//! `HardwareCalibration` type and reach into its fields -- this crate
//! has never seen that type's actual field layout confirmed from a
//! compiled build (only from the `xeb.rs` doc comments), so depending
//! on its exact shape would be guessing. Instead this module
//! re-implements the one published formula `xeb.rs` documents --
//! `p = (1 - F) * d / (d - 1)`, the standard fidelity <-> depolarizing
//! parameter relation from randomized-benchmarking literature -- against
//! the same published Quantinuum Helios numbers (Sandia National
//! Laboratories benchmark, Nature, June 2026), independently of
//! `sirraya_qutub`'s internal representation. If/when `HardwareCalibration`
//! stabilizes as a public, documented API, this can be replaced with a
//! thin wrapper around it instead.
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

impl PublishedCalibration {
    /// Quantinuum Helios (98-qubit trapped-ion), as benchmarked by Sandia
    /// National Laboratories and published in Nature, June 2026 -- the
    /// same figures `sirraya_qutub::xeb::HardwareCalibration` is
    /// documented as using.
    pub fn quantinuum_helios_2026() -> Self {
        Self {
            name: "Quantinuum Helios (Sandia benchmark, Nature, June 2026)",
            single_qubit_fidelity: 0.999975,
            two_qubit_fidelity: 0.99921,
        }
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
