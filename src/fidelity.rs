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
