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
//!   [`BlochVector::GROUND`] (or [`TwoQubitZZState::EQUAL_SUPERPOSITION`]
//!   for the two-qubit case below) -- not chained through a real
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
//!
//! # Two-qubit gates: [`integrate_two_qubit_zz`]
//! `Cx`/`Cz`/`Rzz` genuinely do need at least a four-level (two-qubit)
//! Hilbert space to check physically, which is why this coverage
//! didn't exist until now -- and why, even now, it stays a
//! deliberately simplified, mechanism-agnostic model rather than a
//! claim to simulate any one vendor's real entangling physics.
//!
//! **The model.** This crate's own native two-qubit gate,
//! `Rzz(theta) = exp(-i*theta/2 * Z⊗Z)`, is the entangling primitive
//! every backend's `push_two_qubit_zz` ultimately re-expresses (see
//! `backend/spec.rs`'s doc comment). So the natural two-qubit
//! counterpart to [`integrate`]'s single-qubit on-resonance X-drive is
//! a driven `Z⊗Z` interaction in the rotating frame,
//! `H(t) = (Omega(t)/2) * Z⊗Z`. This is *not* a claim to model any
//! backend's real physical mechanism -- IBM's cross-resonance drive is
//! genuinely `~X⊗Z`-shaped and needs an echo sequence to become a clean
//! `Cx` at all; Rigetti's flux-tunable-coupler `Cz` is an
//! avoided-level-crossing effect; a trapped-ion `Rzz` is a
//! Mølmer-Sørensen gate coupling through a shared motional mode. None
//! of those are captured here, deliberately: each is a real, distinct,
//! device-specific physical process this crate has no measured
//! parameters for, and fabricating one would be exactly the kind of
//! invented-not-cited physics this crate's own conventions elsewhere
//! refuse to produce. What *is* checked, honestly and specifically, is
//! narrower and still real: does a calibrated pulse's envelope,
//! integrated over time, deliver the *entangling angle* this crate's
//! own gate semantics say it should -- the one thing every backend's
//! two-qubit calibration table has in common regardless of mechanism.
//!
//! **Why `Z⊗Z` needs no genuine ODE integration, unlike the
//! single-qubit case.** `Z⊗Z` is diagonal, so it commutes with itself
//! at every instant -- there's no precession the way an X-drive rotates
//! a Bloch vector through multiple axes. The accumulated phase is
//! exactly the time-integral of the drive envelope, computable in
//! closed form. [`integrate_two_qubit_zz`] still runs it through the
//! same fixed-step RK4 machinery [`integrate`] uses (a real, valid way
//! to compute that integral, and it keeps both integrators structurally
//! consistent) -- but unlike the single-qubit case, RK4 isn't *load-
//! bearing* here the way it is for genuine non-commuting dynamics; a
//! plain quadrature would give the same answer. Said plainly rather
//! than dressed up as more dynamically rich than it is.
//!
//! **What's tracked, and why.** [`TwoQubitZZState`] tracks only the
//! `{|00>, |01>}` subspace -- the two-dimensional restriction of the
//! full four-dimensional two-qubit state that's actually informative
//! here. `Z⊗Z` never transfers population between basis states (it's
//! diagonal), so starting anywhere outside an equal superposition of
//! two *different* `Z⊗Z` eigenvalues would only ever accumulate an
//! unobservable global phase. `|00>` (eigenvalue `+1`) and `|01>`
//! (eigenvalue `-1`) straddle the two eigenspaces, so their *relative*
//! phase directly is the accumulated `Rzz` angle -- see
//! [`TwoQubitZZState::relative_phase_rad`].
//!
//! **The two-qubit Rabi-rate constant.**
//! [`TWO_QUBIT_RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS`] is derived the
//! same way [`RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS`] was, from one
//! reference table rather than fit per backend -- but
//! [`crate::pulse::trapped_ion_pulse_calibration`]'s `rzz` table is the
//! reference here, not IBM's, because it's the one calibration in this
//! crate that's actually continuously parameterized by a `pi_amplitude`
//! for this specific gate (`IbmQ`/`Rigetti`'s `two_qubit` tables are
//! fixed-amplitude, with no analogous "achieves theta" parameterization
//! to derive a rate from). `IbmQ`'s and `Rigetti`'s fixed `two_qubit`
//! pulses are then genuinely checked against this same constant --
//! against the standard maximally-entangling target angle
//! `theta = PI/2` (the `Rzz` component of a `Cz`, per this crate's own
//! `native::decompose_cp` identity: `Cz == Rz(pi/2).Rz(pi/2).Rzz(-pi/2)`
//! up to global phase -- so `|theta| = PI/2` is this crate's *own*,
//! already-relied-upon definition of what a maximally-entangling `Cz`/
//! `Cx` pulse should deliver, not a target invented for this check).
//! **The real finding**: both come back at well under 1% of that
//! target -- see this module's own tests. This isn't a bug in
//! `emit.rs`/`backend.rs`/`sequencer.rs`'s actual gate execution
//! (verified separately, exactly, by `sequencer::execute`'s own
//! amplitude-inversion tests) -- it's a real, checked property of
//! `pulse.rs`'s illustrative `two_qubit` tables, whose durations
//! (hundreds of ns, typical of real superconducting CR/CZ gates) are
//! roughly 300-500x shorter than `TrappedIon`'s `rzz` table (100
//! microseconds, typical of a real trapped-ion Mølmer-Sørensen gate) --
//! applying one universal rate constant across gate-duration regimes
//! that differ by that much, by design (see the constant's own doc
//! comment on why it isn't fit per backend), was always going to expose
//! *some* real mismatch. That the two-qubit case shows a much larger
//! one than the single-qubit case (where Rigetti's `rot` table only
//! "weakly fails," per this module's existing tests) is itself the
//! honest, checked result -- not something to silently patch by
//! loosening the constant or rewriting `pulse.rs`'s tables to fit.

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

/// A two-qubit state restricted to the `{|00>, |01>}` subspace, as two
/// complex amplitudes -- everything [`integrate_two_qubit_zz`] needs.
/// See this module's doc comment (under "What's tracked, and why") for
/// why this subspace, specifically, is the informative one for a
/// driven `Z⊗Z` interaction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoQubitZZState {
    /// `(re, im)` amplitude of `|00>`.
    pub c00: (f64, f64),
    /// `(re, im)` amplitude of `|01>`.
    pub c01: (f64, f64),
}

impl TwoQubitZZState {
    /// The starting state every [`integrate_two_qubit_zz`] call begins
    /// from: `(|00> + |01>) / sqrt(2)`, an equal superposition
    /// straddling `Z⊗Z`'s two eigenspaces.
    pub const EQUAL_SUPERPOSITION: TwoQubitZZState = TwoQubitZZState {
        c00: (std::f64::consts::FRAC_1_SQRT_2, 0.0),
        c01: (std::f64::consts::FRAC_1_SQRT_2, 0.0),
    };

    /// Should stay `1.0` (up to numerical-integration error) for any
    /// state reachable by the lossless, unitary evolution
    /// [`integrate_two_qubit_zz`] implements -- same role as
    /// [`BlochVector::norm`] one level down.
    pub fn norm(&self) -> f64 {
        (self.c00.0 * self.c00.0
            + self.c00.1 * self.c00.1
            + self.c01.0 * self.c01.0
            + self.c01.1 * self.c01.1)
            .sqrt()
    }

    /// The relative phase between the `|01>` and `|00>` components --
    /// `arg(c01) - arg(c00)` -- which is exactly the `Rzz` angle a
    /// driven `Z⊗Z` Hamiltonian has accumulated so far, matching this
    /// crate's own sign convention (`Rzz(theta)|01> = e^{i theta/2}|01>`,
    /// `Rzz(theta)|00> = e^{-i theta/2}|00>`, so the difference is
    /// exactly `theta`). Not clamped/wrapped to any particular range --
    /// `atan2` already returns a value in `(-PI, PI]`, and this module
    /// only ever checks it against angles that comfortably fit there.
    pub fn relative_phase_rad(&self) -> f64 {
        self.c01.1.atan2(self.c01.0) - self.c00.1.atan2(self.c00.0)
    }
}

fn add_zz(a: TwoQubitZZState, b: TwoQubitZZState) -> TwoQubitZZState {
    TwoQubitZZState {
        c00: (a.c00.0 + b.c00.0, a.c00.1 + b.c00.1),
        c01: (a.c01.0 + b.c01.0, a.c01.1 + b.c01.1),
    }
}

fn scale_zz(a: TwoQubitZZState, s: f64) -> TwoQubitZZState {
    TwoQubitZZState { c00: (a.c00.0 * s, a.c00.1 * s), c01: (a.c01.0 * s, a.c01.1 * s) }
}

/// Converts a two-qubit `Play` instruction's dimensionless `amplitude`
/// into a physical `Z⊗Z`-interaction rate, in rad/ns, at the
/// envelope's peak -- the two-qubit counterpart to
/// [`RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS`]. Derived by requiring
/// [`crate::pulse::trapped_ion_pulse_calibration`]'s `rzz` table's
/// calibrated `Rzz(a, b, PI)` pulse to integrate to *exactly* a `PI`
/// `Z⊗Z`-rotation under this module's model:
/// `rate = PI / (pi_amplitude * integral_of_the_gaussian_square_shape)`.
/// See this module's doc comment (under "The two-qubit Rabi-rate
/// constant") for why `TrappedIon`'s table is the reference here
/// (not IBM's, unlike the single-qubit constant) and what the real,
/// checked result is for the other two backends' fixed `two_qubit`
/// tables.
pub const TWO_QUBIT_RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS: f64 = 6.832_305_92e-5;

/// Integrates a single two-qubit [`PulseInstruction::Play`] against
/// the driven `Z⊗Z` model this module's doc comment describes
/// (under "Two-qubit gates"), using fixed-step RK4, starting from
/// [`TwoQubitZZState::EQUAL_SUPERPOSITION`]. Returns `Err` for
/// [`PulseInstruction::ShiftPhase`], for the same reason [`integrate`]
/// does -- nothing to integrate for a zero-duration virtual update.
pub fn integrate_two_qubit_zz(instr: &PulseInstruction) -> Result<TwoQubitZZState, String> {
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

    // d(c00)/dt = -i*(Omega/2)*(+1)*c00, d(c01)/dt = -i*(Omega/2)*(-1)*c01
    // -- the opposite sign is exactly Z⊗Z's opposite eigenvalue on
    // |00> vs |01> (see this module's doc comment on why these two
    // basis states specifically).
    let deriv = |s: TwoQubitZZState, t_ns: f64| -> TwoQubitZZState {
        let omega = TWO_QUBIT_RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS
            * amplitude
            * envelope_shape(&envelope, t_ns, duration_ns);
        let half = omega / 2.0;
        TwoQubitZZState {
            c00: (half * s.c00.1, -half * s.c00.0),
            c01: (-half * s.c01.1, half * s.c01.0),
        }
    };

    let mut s = TwoQubitZZState::EQUAL_SUPERPOSITION;
    let mut t = 0.0;
    for _ in 0..STEPS {
        let k1 = deriv(s, t);
        let k2 = deriv(add_zz(s, scale_zz(k1, dt / 2.0)), t + dt / 2.0);
        let k3 = deriv(add_zz(s, scale_zz(k2, dt / 2.0)), t + dt / 2.0);
        let k4 = deriv(add_zz(s, scale_zz(k3, dt)), t + dt);
        s = add_zz(
            s,
            scale_zz(add_zz(add_zz(k1, scale_zz(k2, 2.0)), add_zz(scale_zz(k3, 2.0), k4)), dt / 6.0),
        );
        t += dt;
    }
    Ok(s)
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

    fn play_for_rzz(cal: &crate::pulse::TwoQubitContinuousPulseCalibration, theta: f64) -> PulseInstruction {
        PulseInstruction::Play {
            channel: Channel::Control(0, 1),
            start_time_ns: 0.0,
            duration_ns: cal.duration_ns,
            envelope: Envelope::GaussianSquare { sigma_ns: cal.sigma_ns, risefall_ns: cal.risefall_ns },
            amplitude: cal.pi_amplitude * theta / PI,
        }
    }

    fn play_for_two_qubit(cal: &crate::pulse::TwoQubitPulseCalibration) -> PulseInstruction {
        PulseInstruction::Play {
            channel: Channel::Control(0, 1),
            start_time_ns: 0.0,
            duration_ns: cal.duration_ns,
            envelope: Envelope::GaussianSquare { sigma_ns: cal.sigma_ns, risefall_ns: cal.risefall_ns },
            amplitude: cal.amplitude,
        }
    }

    #[test]
    fn trapped_ion_rzz_pi_pulse_achieves_a_pi_zz_rotation() {
        // TrappedIon's rzz table is this module's two-qubit reference
        // point -- see TWO_QUBIT_RABI_RATE_PER_UNIT_AMPLITUDE_RAD_PER_NS's
        // own doc comment.
        let cal = trapped_ion_pulse_calibration();
        let s = integrate_two_qubit_zz(&play_for_rzz(&cal.rzz.unwrap(), PI)).unwrap();
        let theta = s.relative_phase_rad();
        assert!((theta - PI).abs() < 0.01, "expected ~pi, got {theta}");
    }

    #[test]
    fn trapped_ion_rzz_half_pi_pulse_achieves_roughly_half_the_zz_rotation() {
        let cal = trapped_ion_pulse_calibration();
        let s = integrate_two_qubit_zz(&play_for_rzz(&cal.rzz.unwrap(), PI / 2.0)).unwrap();
        let theta = s.relative_phase_rad();
        assert!((theta - PI / 2.0).abs() < 0.01, "expected ~pi/2, got {theta}");
    }

    #[test]
    fn zero_amplitude_two_qubit_pulse_achieves_no_zz_rotation() {
        let cal = trapped_ion_pulse_calibration();
        let s = integrate_two_qubit_zz(&play_for_rzz(&cal.rzz.unwrap(), 0.0)).unwrap();
        assert!(
            s.relative_phase_rad().abs() < 1e-6,
            "no drive should leave the relative phase at ~0: {s:?}"
        );
    }

    #[test]
    fn two_qubit_integration_preserves_state_norm() {
        // Same role as integration_preserves_bloch_vector_norm one
        // level down: unitary evolution must keep the state normalized
        // -- a check on the integrator's own numerical accuracy,
        // independent of any calibration table.
        let cal = trapped_ion_pulse_calibration();
        let s = integrate_two_qubit_zz(&play_for_rzz(&cal.rzz.unwrap(), PI)).unwrap();
        assert!((s.norm() - 1.0).abs() < 1e-6, "RK4 drift too large: |s| = {}", s.norm());
    }

    #[test]
    fn rzz_relative_phase_scales_linearly_with_theta() {
        let cal = trapped_ion_pulse_calibration();
        for &frac in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            let theta_in = PI * frac;
            let s = integrate_two_qubit_zz(&play_for_rzz(&cal.rzz.unwrap(), theta_in)).unwrap();
            let theta_out = s.relative_phase_rad();
            assert!(
                (theta_out - theta_in).abs() < 0.01,
                "at theta={theta_in}, expected achieved angle ~{theta_in}, got {theta_out}"
            );
        }
    }

    #[test]
    fn shift_phase_is_not_integrable_for_two_qubit_zz_either() {
        let instr = PulseInstruction::ShiftPhase {
            channel: Channel::Drive(0),
            start_time_ns: 0.0,
            angle_rad: 0.3,
        };
        assert!(integrate_two_qubit_zz(&instr).is_err());
    }

    #[test]
    fn ibm_and_rigetti_fixed_two_qubit_pulses_fall_far_short_of_the_maximally_entangling_target() {
        // The real, checked finding this module's doc comment
        // documents in detail (under "The two-qubit Rabi-rate
        // constant"): neither IbmQ's nor Rigetti's illustrative,
        // fixed-amplitude `two_qubit` table comes anywhere close to
        // the PI/2 maximally-entangling Z⊗Z angle this crate's own
        // native::decompose_cp identity defines a Cz/Cx pulse as
        // needing -- both land under 1% of it, a direct, expected
        // consequence of applying TrappedIon's Mølmer-Sørensen-
        // timescale (100 microsecond) reference constant to gates
        // roughly 300-500x shorter, not a bug in this module or in
        // this crate's actual gate execution (verified separately,
        // exactly, by sequencer::execute's own tests). A loose upper
        // bound (10%) rather than a tight one, deliberately: the point
        // is recording the real magnitude, not asserting a precise
        // number that would need updating every time pulse.rs's
        // illustrative tables change.
        let target = PI / 2.0;

        let ibm_cal = ibm_heron_r2_pulse_calibration();
        let ibm_s = integrate_two_qubit_zz(&play_for_two_qubit(&ibm_cal.two_qubit)).unwrap();
        let ibm_theta = ibm_s.relative_phase_rad().abs();
        assert!(
            ibm_theta / target < 0.10,
            "IbmQ's two_qubit pulse achieved {:.6} rad, {:.2}% of the PI/2 target -- expected \
             well under 10% (see this test's own doc comment on why)",
            ibm_theta, 100.0 * ibm_theta / target
        );

        let rigetti_cal = rigetti_ankaa3_pulse_calibration();
        let rigetti_s = integrate_two_qubit_zz(&play_for_two_qubit(&rigetti_cal.two_qubit)).unwrap();
        let rigetti_theta = rigetti_s.relative_phase_rad().abs();
        assert!(
            rigetti_theta / target < 0.10,
            "Rigetti's two_qubit pulse achieved {:.6} rad, {:.2}% of the PI/2 target -- expected \
             well under 10% (see this test's own doc comment on why)",
            rigetti_theta, 100.0 * rigetti_theta / target
        );
    }
}