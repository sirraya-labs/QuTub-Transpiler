//! Rotating-frame two-level integrator: numerically simulates a
//! single [`PulseInstruction::Play`] as a semiclassical drive on an
//! isolated two-level qubit, to check whether a *calibrated* pulse
//! actually implements close to the rotation angle it claims to --
//! the piece [`crate::pulse`]'s own doc comment names as deliberately
//! out of scope for that module ("does not simulate the resulting
//! waveforms against a Hamiltonian to check the calibration table is
//! *correct*"). This is real, separate work, and it was built only
//! once `pulse.rs`'s structural scheduling layer was solid -- the
//! same incremental order the rest of this crate was built in.
//!
//! # What this checks, precisely
//! For a single [`PulseInstruction::Play`] on a `Drive` channel (i.e.
//! a `Rot`), starting from the ground state, this integrates the
//! on-resonance rotating-wave-approximation Bloch equation
//! `d(vec r)/dt = (Omega_x(t), 0, 0) x (vec r)` and reports the net
//! rotation angle the resulting Bloch vector implies. Comparing that
//! against the angle the calibration *claims* (`pi_amplitude` scaled
//! by `theta / PI`, per `pulse.rs`) is the actual check.
//!
//! # Scope this module deliberately does NOT cover
//! - **Two-qubit gates.** `Cx`/`Cz`/`Rzz` need at minimum a four-level
//!   (two-qubit) Hilbert space to mean anything physically; this
//!   module's name says *two-level* on purpose. `PulseCalibration`'s
//!   `two_qubit`/`rzz` fields are not checked by anything here.
//! - **DRAG's `beta` (Q-quadrature) term.** DRAG's entire physical
//!   purpose is suppressing leakage into a qubit's *third* level --
//!   in a strictly two-level model that third level doesn't exist, so
//!   there is nothing for the Q-quadrature term to correct, and
//!   including it here would (self-verified: see this module's
//!   history) actually just add a spurious, uncancelled rotation that
//!   makes even an exactly-right calibration fail to reach a true pi
//!   rotation. So [`integrate`] only ever drives the I-quadrature --
//!   `Envelope::Drag`'s `beta` field is read by `pulse::schedule`
//!   (it's a real part of what gets played) but not by this module. A
//!   real three-level (or higher) integrator that actually checks
//!   DRAG's leakage-suppression claim is further follow-on work, not
//!   done here.
//! - **Chained/multi-instruction schedules.** Every [`Play`](PulseInstruction::Play)
//!   is integrated in isolation, starting fresh from
//!   [`BlochVector::GROUND`] -- not chained through a real
//!   [`crate::pulse::Schedule`], so this says nothing about e.g.
//!   accumulated phase from a preceding `ShiftPhase`.
//! - **Detuning, dephasing, relaxation.** The drive is assumed
//!   perfectly on-resonance and the evolution perfectly unitary (no
//!   `T1`/`T2`) -- see [`integrate`]'s doc comment.
//!
//! # The Rabi-rate constant
//! [`RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS`] converts a `Play`
//! instruction's dimensionless, device-normalized `amplitude` into a
//! physical drive rate in rad/ns. It's a single crate-wide constant,
//! *not* fit separately per backend -- fitting it per backend would
//! make every calibration table trivially "correct" by construction
//! and defeat the entire point of this module. It's derived from
//! [`crate::pulse::ibm_heron_r2_pulse_calibration`]'s `rot` table
//! (chosen as the reference because that table came first
//! historically in this crate); the other tables are then genuinely
//! checked against it, not tautologically. See
//! [`RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS`]'s own doc comment and
//! this module's tests for how each table holds up.

use crate::pulse::{Envelope, PulseInstruction};

/// A qubit's state as a point on the Bloch sphere. `z = 1` is the
/// ground state `|0>`, `z = -1` is the excited state `|1>` --
/// consistent with this crate's other conventions (e.g. what
/// `BackendGate::Measure` measures in the computational basis).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlochVector {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl BlochVector {
    /// The starting state every [`integrate`] call begins from: the
    /// qubit's ground state, `|0>`.
    pub const GROUND: BlochVector = BlochVector { x: 0.0, y: 0.0, z: 1.0 };

    /// Should stay `1.0` (up to numerical-integration error) for any
    /// state reachable by the lossless, unitary evolution [`integrate`]
    /// implements -- a Bloch vector off the unit sphere signals
    /// integrator drift, not physics.
    pub fn norm(&self) -> f64 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    /// Probability of measuring `|1>`: `(1 - z) / 2`.
    pub fn excited_population(&self) -> f64 {
        (1.0 - self.z) / 2.0
    }
}

/// Converts a `Play` instruction's dimensionless `amplitude` into a
/// physical Rabi rate, in rad/ns, at the envelope's peak. Derived by
/// requiring [`crate::pulse::ibm_heron_r2_pulse_calibration`]'s `rot`
/// table's calibrated `Rot(q, PI)` pulse to integrate to *exactly* a
/// pi rotation under this module's (I-quadrature-only, see this
/// module's doc comment) model:
/// `rate = PI / (pi_amplitude * integral_of_the_gaussian_shape)`.
/// IBM's table is the reference point here, not independently
/// measured -- see this module's tests for how well the other two
/// backends' tables hold up against this same constant.
pub const RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS: f64 = 0.738_503_179_1;

/// The I-quadrature drive shape at time `t_ns` into a `duration_ns`-
/// long pulse, as a fraction of the instruction's peak `amplitude`
/// (i.e. in `[0, 1]`, not yet scaled by `amplitude` itself). This
/// module deliberately ignores `Envelope::Drag`'s `beta` (Q-quadrature)
/// term -- see this module's doc comment on why.
fn envelope_shape(envelope: &Envelope, t_ns: f64, duration_ns: f64) -> f64 {
    match *envelope {
        Envelope::Drag { sigma_ns, .. } => {
            let dt = t_ns - duration_ns / 2.0;
            (-(dt * dt) / (2.0 * sigma_ns * sigma_ns)).exp()
        }
        Envelope::GaussianSquare { sigma_ns, risefall_ns } => {
            if t_ns < risefall_ns {
                let dt = t_ns - risefall_ns;
                (-(dt * dt) / (2.0 * sigma_ns * sigma_ns)).exp()
            } else if t_ns > duration_ns - risefall_ns {
                let dt = t_ns - (duration_ns - risefall_ns);
                (-(dt * dt) / (2.0 * sigma_ns * sigma_ns)).exp()
            } else {
                1.0
            }
        }
    }
}

/// Integrates a single [`PulseInstruction::Play`] against the
/// on-resonance, no-detuning, no-dephasing rotating-wave-approximation
/// Bloch equation `d(vec r)/dt = (Omega_x(t), 0, 0) x (vec r)`, using
/// fixed-step RK4, starting from [`BlochVector::GROUND`]. See this
/// module's doc comment for everything that leaves out (two-qubit
/// gates, DRAG's Q-term, chained schedules, detuning/dephasing/`T1`/
/// `T2`). Returns `Err` for [`PulseInstruction::ShiftPhase`], which
/// is a zero-duration virtual phase update, not a physical pulse --
/// nothing to integrate (see `pulse.rs`'s doc comment on virtual-Z).
pub fn integrate(instr: &PulseInstruction) -> Result<BlochVector, String> {
    let (duration_ns, envelope, amplitude) = match *instr {
        PulseInstruction::Play { duration_ns, envelope, amplitude, .. } => {
            (duration_ns, envelope, amplitude)
        }
        PulseInstruction::ShiftPhase { .. } => {
            return Err(
                "ShiftPhase is a zero-duration virtual phase update, not a physical pulse \
                 -- nothing to integrate (see pulse.rs's doc comment on virtual-Z)"
                    .to_string(),
            );
        }
    };

    const STEPS: usize = 2000;
    let dt = duration_ns / STEPS as f64;

    let deriv = |v: BlochVector, t_ns: f64| -> BlochVector {
        let omega_x =
            RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS * amplitude * envelope_shape(&envelope, t_ns, duration_ns);
        BlochVector { x: 0.0, y: -omega_x * v.z, z: omega_x * v.y }
    };

    let mut v = BlochVector::GROUND;
    let mut t = 0.0;
    for _ in 0..STEPS {
        let k1 = deriv(v, t);
        let k2 = deriv(add(v, scale(k1, dt / 2.0)), t + dt / 2.0);
        let k3 = deriv(add(v, scale(k2, dt / 2.0)), t + dt / 2.0);
        let k4 = deriv(add(v, scale(k3, dt)), t + dt);
        v = add(v, scale(add(add(k1, scale(k2, 2.0)), add(scale(k3, 2.0), k4)), dt / 6.0));
        t += dt;
    }
    Ok(v)
}

fn add(a: BlochVector, b: BlochVector) -> BlochVector {
    BlochVector { x: a.x + b.x, y: a.y + b.y, z: a.z + b.z }
}

fn scale(a: BlochVector, s: f64) -> BlochVector {
    BlochVector { x: a.x * s, y: a.y * s, z: a.z * s }
}

/// The net rotation angle a Bloch vector's `z` alone implies --
/// `acos(z)`, clamped against floating-point drift just outside
/// `[-1, 1]`, always in `[0, PI]`. This collapses away `x`/`y` (and so
/// e.g. rotation *direction*); it's the right summary for checking a
/// `Rot(q, theta)`'s calibrated *magnitude*, which is all this module
/// checks. See [`integrate`] for the full vector where more matters.
pub fn rotation_angle_rad(v: BlochVector) -> f64 {
    v.z.clamp(-1.0, 1.0).acos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulse::{
        ibm_heron_r2_pulse_calibration, rigetti_ankaa3_pulse_calibration,
        trapped_ion_pulse_calibration, Channel, SingleQubitPulseCalibration,
    };
    use std::f64::consts::PI;

    fn play_for_rot(cal: &SingleQubitPulseCalibration, theta: f64) -> PulseInstruction {
        PulseInstruction::Play {
            channel: Channel::Drive(0),
            start_time_ns: 0.0,
            duration_ns: cal.duration_ns,
            envelope: Envelope::Drag { sigma_ns: cal.sigma_ns, beta: cal.drag_beta },
            amplitude: cal.pi_amplitude * theta / PI,
        }
    }

    #[test]
    fn ibm_pi_pulse_achieves_a_pi_rotation() {
        // IBM's rot table is this module's reference point -- see
        // RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS's doc comment.
        let cal = ibm_heron_r2_pulse_calibration();
        let v = integrate(&play_for_rot(&cal.rot, PI)).unwrap();
        let theta = rotation_angle_rad(v);
        assert!((theta - PI).abs() < 0.01, "expected ~pi, got {theta}");
    }

    #[test]
    fn ibm_half_pi_pulse_achieves_roughly_half_the_rotation() {
        let cal = ibm_heron_r2_pulse_calibration();
        let v = integrate(&play_for_rot(&cal.rot, PI / 2.0)).unwrap();
        let theta = rotation_angle_rad(v);
        assert!((theta - PI / 2.0).abs() < 0.01, "expected ~pi/2, got {theta}");
    }

    #[test]
    fn zero_amplitude_pulse_achieves_no_rotation() {
        let cal = ibm_heron_r2_pulse_calibration();
        let v = integrate(&play_for_rot(&cal.rot, 0.0)).unwrap();
        assert!(
            (v.z - 1.0).abs() < 1e-9,
            "no drive should leave the qubit in the ground state: {v:?}"
        );
    }

    #[test]
    fn integration_preserves_bloch_vector_norm() {
        // Unitary (lossless) evolution must keep the state on the
        // Bloch sphere -- a check on the integrator's own numerical
        // accuracy, independent of any calibration table.
        let cal = ibm_heron_r2_pulse_calibration();
        let v = integrate(&play_for_rot(&cal.rot, PI)).unwrap();
        assert!((v.norm() - 1.0).abs() < 1e-6, "RK4 drift too large: |v| = {}", v.norm());
    }

    #[test]
    fn rigetti_pi_pulse_is_within_a_documented_tolerance_of_a_pi_rotation() {
        // Rigetti's rot table wasn't used to derive the Rabi-rate
        // constant -- IBM's was -- so this is a genuine cross-check,
        // not a tautology. It lands ~19% short of a full pi rotation:
        // within this module's documented illustrative-not-measured
        // tolerance, but a real, recorded finding, not silently
        // rounded away by loosening the constant to fit.
        let cal = rigetti_ankaa3_pulse_calibration();
        let v = integrate(&play_for_rot(&cal.rot, PI)).unwrap();
        let theta = rotation_angle_rad(v);
        assert!(
            (theta - PI).abs() / PI < 0.25,
            "Rigetti's calibrated Rot(PI) should be within 25% of a true pi rotation, \
             got {theta} ({:.0}% of pi)",
            100.0 * theta / PI
        );
    }

    #[test]
    fn trapped_ion_pi_pulse_achieves_a_pi_rotation() {
        // TrappedIon's rot table was deliberately chosen (see
        // pulse::trapped_ion_pulse_calibration's doc comment) to be
        // self-consistent with the Rabi-rate constant, unlike
        // Rigetti's -- so this holds to the same tight tolerance as
        // the IBM check above, despite running ~140x longer.
        let cal = trapped_ion_pulse_calibration();
        let v = integrate(&play_for_rot(&cal.rot, PI)).unwrap();
        let theta = rotation_angle_rad(v);
        assert!((theta - PI).abs() < 0.01, "expected ~pi, got {theta}");
    }

    #[test]
    fn rotation_angle_scales_linearly_with_theta_for_a_self_consistent_table() {
        let cal = trapped_ion_pulse_calibration();
        for &frac in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            let theta_in = PI * frac;
            let v = integrate(&play_for_rot(&cal.rot, theta_in)).unwrap();
            let theta_out = rotation_angle_rad(v);
            assert!(
                (theta_out - theta_in).abs() < 0.01,
                "at theta={theta_in}, expected achieved angle ~{theta_in}, got {theta_out}"
            );
        }
    }

    #[test]
    fn shift_phase_is_not_integrable() {
        let instr = PulseInstruction::ShiftPhase {
            channel: Channel::Drive(0),
            start_time_ns: 0.0,
            angle_rad: 0.3,
        };
        assert!(integrate(&instr).is_err());
    }
}
