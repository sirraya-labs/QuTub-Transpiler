//! Pulse-level scheduling: lowers an already-lowered, already-optimized
//! [`BackendCircuit`] (see `backend.rs`) into a [`Schedule`] of
//! hardware-channel pulse instructions, using a per-backend calibration
//! table.
//!
//! This module sits strictly *below* everything else in the crate: it
//! only ever reads a `BackendCircuit`, the final output of
//! `backend::lower`. Nothing upstream of that -- `ir_optimize.rs`,
//! `route.rs`, `backend::optimize`/`resynthesize` -- has to know this
//! module exists, and none of it changes to support this. If this
//! module were deleted entirely, every other test in the crate would
//! still pass unchanged.
//!
//! # Scope of this first pass
//! This implements *structural* pulse scheduling only: an ASAP
//! (as-soon-as-possible) list scheduler that assigns each gate a start
//! time and a hardware channel from a calibration table, and enforces
//! the invariants that make a schedule physically sane (no two pulses
//! overlapping on the same channel, causally consistent per-qubit
//! ordering). It does **not** simulate the resulting waveforms against
//! a Hamiltonian to check the calibration table is *correct* -- that
//! is [`crate::waveform_sim`]'s job, a separate module built once this
//! structural layer was solid, the same incremental order the rest of
//! this crate was built in. `waveform_sim` only checks
//! [`SingleQubitPulseCalibration`]/`Rot` (a genuinely two-level
//! problem); it has nothing to say about `two_qubit`/`rzz`, which
//! would need a larger Hilbert space -- see its own doc comment.
//!
//! # Virtual-Z
//! On real superconducting hardware, `Rz` is essentially never a
//! physical pulse -- it's implemented as a *virtual* phase-frame
//! update: a zero-duration bookkeeping change to the phase reference
//! every later pulse on that channel is measured against, exact by
//! construction (see e.g. McKay et al., "Efficient Z gates for
//! quantum computing", 2017). Modeling it any other way would both
//! waste schedule duration on a gate that costs nothing on real
//! hardware, and misstate its (zero) contribution to any pulse-level
//! error budget. [`PulseInstruction::ShiftPhase`] carries a
//! `start_time_ns` purely for causal ordering in a schedule listing --
//! it never advances a qubit's busy-until time, which [`schedule`]
//! enforces by construction (see its loop body).
//!
//! # Backend coverage
//! [`Backend::IbmQ`] ([`ibm_heron_r2_pulse_calibration`]),
//! [`Backend::Rigetti`] ([`rigetti_ankaa3_pulse_calibration`]), and
//! [`Backend::TrappedIon`] ([`trapped_ion_pulse_calibration`]) all
//! have calibration tables and are wired through [`schedule`] --
//! `schedule`'s `Cx`/`Cz` handling was already backend-agnostic (one
//! match arm, `BackendGate::Cx(a,b) | BackendGate::Cz(a,b)`), so
//! adding `Rigetti` needed no change to `schedule` itself, only a new
//! calibration table. `TrappedIon`'s native `Rzz` needed one new match
//! arm plus a new calibration shape -- unlike `Cx`/`Cz`, which are
//! fixed-angle gates calibrated once, `Rzz(a, b, theta)` is
//! continuously-variable, so it's calibrated the same way `Rot` is
//! ([`TwoQubitContinuousPulseCalibration::pi_amplitude`], scaled
//! linearly by `theta / PI`) rather than the way `Cx`/`Cz` are
//! ([`TwoQubitPulseCalibration`], one fixed pulse). `PulseCalibration::rzz`
//! is `None` for `IbmQ`/`Rigetti` -- backends whose native two-qubit
//! gate is fixed-angle have nothing to put there -- so [`schedule`]
//! still returns `Err` rather than guessing if an `Rzz` ever reaches a
//! calibration table that has no entry for it, including a
//! calibration/circuit backend mismatch.
//!
//! # On the calibration numbers themselves
//! [`ibm_heron_r2_pulse_calibration`]'s duration/amplitude/sigma
//! values are illustrative -- modeled after the right *order of
//! magnitude* for real superconducting hardware (tens of ns for a
//! single-qubit pulse, hundreds of ns for a two-qubit pulse,
//! microseconds for readout) -- not measured vendor data, the same
//! caveat `backend.rs`'s `Rot`-as-continuous-rotation modeling
//! simplification already carries. Calibration here is also uniform
//! across every qubit/pair, a further simplification real hardware
//! doesn't share (every qubit is calibrated independently in
//! practice); swapping in real, per-qubit numbers later only touches
//! [`PulseCalibration`]'s construction, not [`schedule`]'s logic.

use crate::backend::{Backend, BackendCircuit, BackendGate};
use std::collections::HashMap;
use std::f64::consts::PI;

/// A hardware channel a pulse instruction plays on -- not a qubit
/// index, though it's derived from one (or two). Real backends give
/// each qubit its own drive line and give each coupled qubit pair
/// (for cross-resonance-style gates) a control line on top of that;
/// `Measure` gets its own dedicated readout line per qubit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Single-qubit drive line for qubit `q` -- carries `Rot` pulses
    /// and `Rz`'s virtual phase-frame updates.
    Drive(usize),
    /// Two-qubit control line for the ordered pair `(a, b)` exactly as
    /// it appeared in the source `BackendGate` (order matters for
    /// `Cx`: `(control, target)`).
    Control(usize, usize),
    /// Readout line for qubit `q`.
    Readout(usize),
}

/// A pulse envelope shape. `Drag` (Derivative Removal by Adiabatic
/// Gate) is what real superconducting single-qubit gates almost
/// always use -- a Gaussian with a derivative correction term that
/// suppresses leakage into the qubit's second excited state; `beta`
/// is that correction's weight. `GaussianSquare` (flat-top Gaussian)
/// is the shape cross-resonance-style two-qubit pulses, and readout
/// pulses, typically use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Envelope {
    Drag { sigma_ns: f64, beta: f64 },
    GaussianSquare { sigma_ns: f64, risefall_ns: f64 },
}

/// One instruction in a [`Schedule`]: either a physical pulse to play,
/// or a virtual (zero-duration) phase-frame shift. `start_time_ns` is
/// absolute, measured from the start of the schedule.
#[derive(Debug, Clone, PartialEq)]
pub enum PulseInstruction {
    Play {
        channel: Channel,
        start_time_ns: f64,
        duration_ns: f64,
        envelope: Envelope,
        /// Device-normalized amplitude, dimensionless.
        amplitude: f64,
    },
    ShiftPhase {
        channel: Channel,
        start_time_ns: f64,
        angle_rad: f64,
    },
}

impl PulseInstruction {
    pub fn channel(&self) -> Channel {
        match *self {
            PulseInstruction::Play { channel, .. } => channel,
            PulseInstruction::ShiftPhase { channel, .. } => channel,
        }
    }

    pub fn start_time_ns(&self) -> f64 {
        match *self {
            PulseInstruction::Play { start_time_ns, .. } => start_time_ns,
            PulseInstruction::ShiftPhase { start_time_ns, .. } => start_time_ns,
        }
    }

    /// End time on this instruction's channel -- equal to
    /// `start_time_ns` for `ShiftPhase`, since it's zero-duration by
    /// construction (see this module's doc comment on virtual-Z).
    pub fn end_time_ns(&self) -> f64 {
        match *self {
            PulseInstruction::Play { start_time_ns, duration_ns, .. } => start_time_ns + duration_ns,
            PulseInstruction::ShiftPhase { start_time_ns, .. } => start_time_ns,
        }
    }
}

/// A scheduled pulse program for one [`BackendCircuit`], on one
/// backend's real hardware channels.
#[derive(Debug, Clone)]
pub struct Schedule {
    pub backend: Backend,
    pub instructions: Vec<PulseInstruction>,
}

impl Schedule {
    /// The schedule's total makespan: the latest end time across every
    /// instruction. `ShiftPhase` never determines this on its own --
    /// it has zero duration -- but is still included in the `map` for
    /// uniformity; it just never wins the `max`.
    pub fn duration_ns(&self) -> f64 {
        self.instructions
            .iter()
            .map(PulseInstruction::end_time_ns)
            .fold(0.0, f64::max)
    }

    /// `true` if no two `Play` instructions on the same channel overlap
    /// in time -- the structural invariant a schedule must satisfy to
    /// be physically realizable at all, regardless of whether the
    /// calibration numbers behind it are accurate. `ShiftPhase` is
    /// zero-duration and excluded: two frame updates "at the same
    /// instant" on the same channel are just sequential bookkeeping,
    /// not a physical conflict.
    pub fn has_no_overlaps(&self) -> bool {
        let mut by_channel: HashMap<Channel, Vec<(f64, f64)>> = HashMap::new();
        for instr in &self.instructions {
            if let PulseInstruction::Play { channel, start_time_ns, duration_ns, .. } = *instr {
                by_channel
                    .entry(channel)
                    .or_default()
                    .push((start_time_ns, start_time_ns + duration_ns));
            }
        }
        for windows in by_channel.values_mut() {
            windows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            for pair in windows.windows(2) {
                let (_, end_a) = pair[0];
                let (start_b, _) = pair[1];
                if start_b < end_a - 1e-9 {
                    return false;
                }
            }
        }
        true
    }
}

/// Calibrated pulse parameters for a backend's native continuously-
/// variable single-qubit rotation (`BackendGate::Rot` -- `Rx` on
/// `IbmQ`/`Rigetti`, `Ry` on `TrappedIon`; see `backend.rs`). Amplitude
/// scales linearly with the rotation angle from a calibrated pi-pulse
/// -- the same modeling simplification `backend.rs` already makes by
/// representing a fixed physical `SX` pulse as a continuously-
/// parameterized rotation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SingleQubitPulseCalibration {
    pub duration_ns: f64,
    pub sigma_ns: f64,
    pub drag_beta: f64,
    /// Amplitude of a calibrated `Rot(q, PI)` pulse. `Rot(q, theta)`
    /// scales this linearly: `amplitude(theta) = pi_amplitude * theta / PI`.
    pub pi_amplitude: f64,
}

/// Calibrated pulse parameters for a backend's native two-qubit gate
/// (`Cx` on `IbmQ`, `Cz` on `Rigetti`; see `backend.rs`). Unlike the
/// single-qubit case, this crate does not model a continuously-
/// variable two-qubit angle -- both `Cx` and `Cz` are fixed gates --
/// so there's no angle-to-amplitude scaling here, just one calibrated
/// pulse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoQubitPulseCalibration {
    pub duration_ns: f64,
    pub sigma_ns: f64,
    pub risefall_ns: f64,
    pub amplitude: f64,
}

/// Calibrated pulse parameters for a backend's native continuously-
/// variable two-qubit gate (`Rzz` on `TrappedIon`; see `backend.rs`).
/// Unlike [`TwoQubitPulseCalibration`] (`Cx`/`Cz`, both fixed-angle),
/// `Rzz(a, b, theta)` is continuously-variable in `theta`, the same
/// way `Rot` is -- so this scales the same way
/// [`SingleQubitPulseCalibration`] does: amplitude linear in the
/// rotation angle from a calibrated pi-pulse, not a single fixed
/// pulse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoQubitContinuousPulseCalibration {
    pub duration_ns: f64,
    pub sigma_ns: f64,
    pub risefall_ns: f64,
    /// Amplitude of a calibrated `Rzz(a, b, PI)` pulse. `Rzz(a, b, theta)`
    /// scales this linearly: `amplitude(theta) = pi_amplitude * theta / PI`.
    pub pi_amplitude: f64,
}

/// Per-backend calibration data [`schedule`] looks up gate pulses
/// from. Uniform across every qubit and every qubit pair for now --
/// see this module's doc comment on why that's a named simplification,
/// not a hardware fact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PulseCalibration {
    pub backend: Backend,
    pub rot: SingleQubitPulseCalibration,
    pub two_qubit: TwoQubitPulseCalibration,
    /// Calibration for `BackendGate::Rzz`, `TrappedIon`'s native
    /// continuously-variable two-qubit gate. `None` for `IbmQ` and
    /// `Rigetti`, whose native two-qubit gate is the fixed-angle
    /// `Cx`/`Cz` covered by `two_qubit` instead -- there's nothing to
    /// calibrate here for them. [`schedule`] returns `Err` for an
    /// `Rzz` gate against a calibration table where this is `None`.
    pub rzz: Option<TwoQubitContinuousPulseCalibration>,
    pub readout_duration_ns: f64,
}

/// A first-pass calibration table for [`Backend::IbmQ`], modeled after
/// the same real hardware family `Backend::calibration`'s
/// `crate::fidelity::PublishedCalibration::ibm_heron_r2()` targets.
/// See this module's doc comment for why these particular numbers are
/// illustrative rather than measured. This table's `rot` is also the
/// reference point [`crate::waveform_sim`]'s Rabi-rate constant is
/// derived from -- see that module's doc comment.
pub fn ibm_heron_r2_pulse_calibration() -> PulseCalibration {
    PulseCalibration {
        backend: Backend::IbmQ,
        rot: SingleQubitPulseCalibration {
            duration_ns: 35.56,
            sigma_ns: 8.89,
            drag_beta: -1.2,
            pi_amplitude: 0.2,
        },
        two_qubit: TwoQubitPulseCalibration {
            duration_ns: 300.0,
            sigma_ns: 16.0,
            risefall_ns: 32.0,
            amplitude: 0.4,
        },
        rzz: None,
        readout_duration_ns: 3300.0,
    }
}

/// A first-pass calibration table for [`Backend::Rigetti`], modeled
/// after the same real hardware family `Backend::calibration`'s
/// `crate::fidelity::PublishedCalibration::rigetti_ankaa3()` targets.
/// Same illustrative-not-measured caveat as
/// [`ibm_heron_r2_pulse_calibration`]; the two differ because the
/// underlying gate physics differs, not by accident: `Rigetti`'s `Cz`
/// is a flux-tunable coupler gate (typically shorter than IBM's
/// cross-resonance `Cx` on real Ankaa-class hardware), so `two_qubit`
/// here carries a materially shorter `duration_ns` than the IBM table
/// -- this is the calibration-table-only difference the module doc
/// comment on backend coverage describes.
pub fn rigetti_ankaa3_pulse_calibration() -> PulseCalibration {
    PulseCalibration {
        backend: Backend::Rigetti,
        rot: SingleQubitPulseCalibration {
            duration_ns: 32.0,
            sigma_ns: 8.0,
            drag_beta: -1.0,
            pi_amplitude: 0.18,
        },
        two_qubit: TwoQubitPulseCalibration {
            duration_ns: 180.0,
            sigma_ns: 10.0,
            risefall_ns: 20.0,
            amplitude: 0.35,
        },
        rzz: None,
        readout_duration_ns: 800.0,
    }
}

/// A first-pass calibration table for [`Backend::TrappedIon`]. Same
/// illustrative-not-measured caveat as [`ibm_heron_r2_pulse_calibration`]
/// and [`rigetti_ankaa3_pulse_calibration`] -- order-of-magnitude only.
/// Trapped-ion gates are physically slower than superconducting ones
/// (motional-mode-mediated entangling gates and Raman-transition
/// single-qubit gates, not fixed-frequency microwave/flux pulses), so
/// both `rot` and `rzz` here carry materially longer `duration_ns`
/// than either superconducting table: microseconds rather than
/// tens/hundreds of nanoseconds. `two_qubit` (`Cx`/`Cz`) is populated
/// for structural completeness but isn't exercised by a real
/// `TrappedIon`-lowered circuit -- `backend::lower` for `TrappedIon`
/// produces `Rzz`, not `Cx`/`Cz` (see this module's doc comment on
/// backend coverage). `rot.drag_beta` is `0.0`: DRAG is specifically a
/// superconducting-transmon leakage-suppression technique (see this
/// module's doc comment on Drag) and doesn't apply to a Raman-driven
/// trapped-ion qubit -- and, unlike `rot.pi_amplitude` below, that's
/// not a number [`crate::waveform_sim`] checks, just a fact about the
/// hardware. `rot.pi_amplitude` *was* chosen so that a calibrated
/// `Rot(q, PI)` actually integrates to a pi rotation under
/// `waveform_sim`'s model, at this `duration_ns`/`sigma_ns` -- unlike
/// [`rigetti_ankaa3_pulse_calibration`]'s `rot`, which weakly fails
/// that same check (see `waveform_sim`'s tests). Both are "illustrative,
/// not measured" in the same sense, but only one of the two is also
/// internally self-consistent; that inconsistency in Rigetti's numbers
/// is a real, recorded finding, not a bug in `waveform_sim`.
pub fn trapped_ion_pulse_calibration() -> PulseCalibration {
    PulseCalibration {
        backend: Backend::TrappedIon,
        rot: SingleQubitPulseCalibration {
            duration_ns: 5000.0,
            sigma_ns: 1250.0,
            drag_beta: 0.0,
            pi_amplitude: 0.0014224,
        },
        two_qubit: TwoQubitPulseCalibration {
            duration_ns: 100_000.0,
            sigma_ns: 5000.0,
            risefall_ns: 10_000.0,
            amplitude: 0.5,
        },
        rzz: Some(TwoQubitContinuousPulseCalibration {
            duration_ns: 100_000.0,
            sigma_ns: 5000.0,
            risefall_ns: 10_000.0,
            pi_amplitude: 0.5,
        }),
        readout_duration_ns: 200_000.0,
    }
}

/// Lowers `bc` into a [`Schedule`] against `cal`, an ASAP list
/// scheduler: each gate starts as soon as every qubit it touches is
/// free, tracked per-qubit via `busy_until`. Two gates on disjoint
/// qubits therefore end up scheduled in parallel (different start
/// times only if one's qubit was already busy from something earlier
/// in program order); two gates sharing a qubit are serialized.
///
/// Returns `Err` rather than guessing for anything this scheduler
/// doesn't know how to handle: a calibration/circuit backend
/// mismatch, or a `BackendGate::Rzz` against a calibration table
/// whose `rzz` field is `None` (true of [`ibm_heron_r2_pulse_calibration`]
/// and [`rigetti_ankaa3_pulse_calibration`] -- see this module's doc
/// comment on backend coverage).
pub fn schedule(bc: &BackendCircuit, cal: &PulseCalibration) -> Result<Schedule, String> {
    if bc.backend != cal.backend {
        return Err(format!(
            "calibration is for {:?} but circuit was lowered for {:?}",
            cal.backend, bc.backend
        ));
    }

    let mut busy_until: HashMap<usize, f64> = HashMap::new();
    let mut instructions = Vec::with_capacity(bc.gates.len());

    for g in &bc.gates {
        schedule_one(g, cal, &mut busy_until, &mut instructions)?;
    }

    Ok(Schedule { backend: bc.backend, instructions })
}

/// One gate's contribution to a [`Schedule`] -- the per-gate body
/// [`schedule`]'s loop used to inline directly, factored out so
/// `BackendGate::If` can recurse into its `inner` with the same logic
/// rather than a second copy of it.
fn schedule_one(
    g: &BackendGate,
    cal: &PulseCalibration,
    busy_until: &mut HashMap<usize, f64>,
    instructions: &mut Vec<PulseInstruction>,
) -> Result<(), String> {
    fn qubit_time(q: usize, busy_until: &HashMap<usize, f64>) -> f64 {
        *busy_until.get(&q).unwrap_or(&0.0)
    }

    match *g {
        BackendGate::Rz(q, theta) => {
            let t = qubit_time(q, busy_until);
            instructions.push(PulseInstruction::ShiftPhase {
                channel: Channel::Drive(q),
                start_time_ns: t,
                angle_rad: theta,
            });
            // Zero duration: `busy_until[q]` is deliberately left
            // untouched -- see this module's doc comment on
            // virtual-Z.
        }
        BackendGate::Rot(q, theta) => {
            let t = qubit_time(q, busy_until);
            let amplitude = cal.rot.pi_amplitude * theta / PI;
            instructions.push(PulseInstruction::Play {
                channel: Channel::Drive(q),
                start_time_ns: t,
                duration_ns: cal.rot.duration_ns,
                envelope: Envelope::Drag { sigma_ns: cal.rot.sigma_ns, beta: cal.rot.drag_beta },
                amplitude,
            });
            busy_until.insert(q, t + cal.rot.duration_ns);
        }
        BackendGate::Cx(a, b) | BackendGate::Cz(a, b) => {
            let t = qubit_time(a, busy_until).max(qubit_time(b, busy_until));
            instructions.push(PulseInstruction::Play {
                channel: Channel::Control(a, b),
                start_time_ns: t,
                duration_ns: cal.two_qubit.duration_ns,
                envelope: Envelope::GaussianSquare {
                    sigma_ns: cal.two_qubit.sigma_ns,
                    risefall_ns: cal.two_qubit.risefall_ns,
                },
                amplitude: cal.two_qubit.amplitude,
            });
            let end = t + cal.two_qubit.duration_ns;
            busy_until.insert(a, end);
            busy_until.insert(b, end);
        }
        BackendGate::Rzz(a, b, theta) => {
            let rzz_cal = cal.rzz.ok_or_else(|| {
                format!(
                    "pulse scheduling for Rzz (TrappedIon's native two-qubit gate, on \
                     qubits {a},{b}) has no calibration entry on backend {:?} -- see \
                     this module's doc comment on backend coverage",
                    cal.backend
                )
            })?;
            let t = qubit_time(a, busy_until).max(qubit_time(b, busy_until));
            let amplitude = rzz_cal.pi_amplitude * theta / PI;
            instructions.push(PulseInstruction::Play {
                channel: Channel::Control(a, b),
                start_time_ns: t,
                duration_ns: rzz_cal.duration_ns,
                envelope: Envelope::GaussianSquare {
                    sigma_ns: rzz_cal.sigma_ns,
                    risefall_ns: rzz_cal.risefall_ns,
                },
                amplitude,
            });
            let end = t + rzz_cal.duration_ns;
            busy_until.insert(a, end);
            busy_until.insert(b, end);
        }
        BackendGate::Measure(q, _c) => {
            let t = qubit_time(q, busy_until);
            let duration = cal.readout_duration_ns;
            instructions.push(PulseInstruction::Play {
                channel: Channel::Readout(q),
                start_time_ns: t,
                duration_ns: duration,
                envelope: Envelope::GaussianSquare {
                    sigma_ns: duration / 8.0,
                    risefall_ns: duration / 8.0,
                },
                amplitude: 1.0,
            });
            busy_until.insert(q, t + duration);
        }
        BackendGate::If(_, _, ref inner) => {
            // Scheduled exactly as `inner` would be on its own --
            // *this* static `Schedule` only ever describes which
            // pulses exist and when, never whether one actually fires
            // at runtime (see `emit.rs`'s execution of this variant,
            // which is what makes that call). That split mirrors how
            // this crate already treats the analogous case one layer
            // up: `backend::lower`'s `If` handling compiles the pulse
            // unconditionally into the schedule; the real-time
            // classical feed-forward decision belongs to control
            // electronics this static model doesn't represent, not to
            // schedule construction. A genuine dynamic-circuits
            // scheduler -- one that can express "wait for the
            // classical result, then branch, with real feed-forward
            // latency" -- is real, separate follow-on work this
            // function doesn't attempt.
            schedule_one(inner, cal, busy_until, instructions)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend;
    use crate::ir::{Circuit, Gate};

    fn cal() -> PulseCalibration {
        ibm_heron_r2_pulse_calibration()
    }

    #[test]
    fn backend_mismatch_returns_error_not_panic() {
        let bc = BackendCircuit {
            backend: Backend::TrappedIon,
            num_qubits: 1,
            num_clbits: 0,
            gates: vec![],
        };
        let err = schedule(&bc, &cal()).unwrap_err();
        assert!(
            err.contains("TrappedIon") && err.contains("IbmQ"),
            "error should name both the calibration's and the circuit's backend, got: {err}"
        );
    }

    #[test]
    fn rzz_returns_error_not_panic() {
        // Rzz never legitimately appears on an IbmQ-backend
        // BackendCircuit (push_rzz rewrites it to Cx/Rz/Cx in
        // backend.rs), but the scheduler must fail loudly rather than
        // silently mis-schedule it if it ever did.
        let bc = BackendCircuit {
            backend: Backend::IbmQ,
            num_qubits: 2,
            num_clbits: 0,
            gates: vec![BackendGate::Rzz(0, 1, 0.4)],
        };
        let err = schedule(&bc, &cal()).unwrap_err();
        assert!(err.contains("Rzz"), "error should name the unsupported gate, got: {err}");
    }

    #[test]
    fn disjoint_qubit_rots_schedule_in_parallel() {
        let bc = BackendCircuit {
            backend: Backend::IbmQ,
            num_qubits: 2,
            num_clbits: 0,
            gates: vec![BackendGate::Rot(0, PI), BackendGate::Rot(1, PI)],
        };
        let sched = schedule(&bc, &cal()).unwrap();
        assert!(sched.has_no_overlaps());
        assert_eq!(
            sched.duration_ns(),
            cal().rot.duration_ns,
            "two Rot's on different qubits should overlap in time (parallel), not sum"
        );
    }

    #[test]
    fn same_qubit_rots_serialize() {
        let bc = BackendCircuit {
            backend: Backend::IbmQ,
            num_qubits: 1,
            num_clbits: 0,
            gates: vec![BackendGate::Rot(0, PI), BackendGate::Rot(0, PI)],
        };
        let sched = schedule(&bc, &cal()).unwrap();
        assert!(sched.has_no_overlaps());
        assert_eq!(
            sched.duration_ns(),
            2.0 * cal().rot.duration_ns,
            "two Rot's on the SAME qubit must serialize, not overlap"
        );
    }

    #[test]
    fn virtual_z_does_not_advance_time() {
        // Rz(0, t) . Rot(0, PI): the Rz must not push the Rot's start
        // time forward at all -- it's a zero-duration frame update,
        // not a physical pulse (see this module's doc comment).
        let bc = BackendCircuit {
            backend: Backend::IbmQ,
            num_qubits: 1,
            num_clbits: 0,
            gates: vec![BackendGate::Rz(0, 0.3), BackendGate::Rot(0, PI)],
        };
        let sched = schedule(&bc, &cal()).unwrap();
        let rot = sched
            .instructions
            .iter()
            .find(|i| matches!(i, PulseInstruction::Play { .. }))
            .unwrap();
        assert_eq!(
            rot.start_time_ns(),
            0.0,
            "the Rot must start at t=0, unaffected by the preceding virtual-Z: {rot:?}"
        );
        assert_eq!(sched.duration_ns(), cal().rot.duration_ns);
    }

    #[test]
    fn entangling_gate_occupies_both_qubits() {
        // Cx(0,1) . Rot(0, PI): the Rot must not start until the Cx
        // (which busies BOTH qubits, not just its control) has ended.
        let bc = BackendCircuit {
            backend: Backend::IbmQ,
            num_qubits: 2,
            num_clbits: 0,
            gates: vec![BackendGate::Cx(0, 1), BackendGate::Rot(0, PI)],
        };
        let sched = schedule(&bc, &cal()).unwrap();
        assert!(sched.has_no_overlaps());
        let rot = sched
            .instructions
            .iter()
            .find(|i| matches!(i, PulseInstruction::Play { channel: Channel::Drive(0), .. }))
            .unwrap();
        assert_eq!(
            rot.start_time_ns(),
            cal().two_qubit.duration_ns,
            "Rot(0) must wait for the Cx to finish on qubit 0: {rot:?}"
        );
    }

    #[test]
    fn measure_gets_a_dedicated_readout_channel() {
        let bc = BackendCircuit {
            backend: Backend::IbmQ,
            num_qubits: 1,
            num_clbits: 1,
            gates: vec![BackendGate::Rot(0, PI), BackendGate::Measure(0, 0)],
        };
        let sched = schedule(&bc, &cal()).unwrap();
        assert!(sched.has_no_overlaps());
        let readout = sched
            .instructions
            .iter()
            .find(|i| i.channel() == Channel::Readout(0))
            .expect("Measure should produce a Readout(0) instruction");
        assert_eq!(readout.start_time_ns(), cal().rot.duration_ns);
        assert_eq!(sched.duration_ns(), cal().rot.duration_ns + cal().readout_duration_ns);
    }

    #[test]
    fn schedules_a_real_lowered_circuit_without_overlaps() {
        // End-to-end: Circuit -> backend::lower(IbmQ) -> schedule,
        // exactly the pipeline this module's doc comment describes --
        // nothing in ir.rs or backend.rs had to change for this to work.
        let mut c = Circuit::new(2);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 1))
            .push(Gate::Rz(1, 0.3))
            .push(Gate::Measure(0, 0))
            .push(Gate::Measure(1, 1));
        c.num_clbits = 2;

        let bc = backend::lower(&c, Backend::IbmQ);
        let sched = schedule(&bc, &cal()).unwrap();
        assert!(
            sched.has_no_overlaps(),
            "schedule for a real lowered circuit must not overlap: {:?}",
            sched.instructions
        );
        assert!(sched.duration_ns() > 0.0);
    }

    #[test]
    fn schedules_a_real_rigetti_lowered_circuit_without_overlaps() {
        // Same end-to-end shape as the IbmQ test above, but through
        // Backend::Rigetti's Cz-native path -- confirms `schedule`'s
        // backend-agnostic Cx/Cz handling actually holds up against
        // the real lowering pipeline, not just hand-built BackendGate
        // vectors.
        let mut c = Circuit::new(2);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 1))
            .push(Gate::Rz(1, 0.3))
            .push(Gate::Measure(0, 0))
            .push(Gate::Measure(1, 1));
        c.num_clbits = 2;

        let bc = backend::lower(&c, Backend::Rigetti);
        assert!(
            bc.gates.iter().any(|g| matches!(g, BackendGate::Cz(..))),
            "sanity check: Rigetti's lowering should actually use Cz, or this test isn't \
             exercising the path it claims to"
        );
        let sched = schedule(&bc, &rigetti_ankaa3_pulse_calibration()).unwrap();
        assert!(
            sched.has_no_overlaps(),
            "schedule for a real Rigetti-lowered circuit must not overlap: {:?}",
            sched.instructions
        );
        assert!(sched.duration_ns() > 0.0);
    }

    #[test]
    fn trapped_ion_calibration_schedules_rzz() {
        let bc = BackendCircuit {
            backend: Backend::TrappedIon,
            num_qubits: 2,
            num_clbits: 0,
            gates: vec![BackendGate::Rzz(0, 1, PI)],
        };
        let tcal = trapped_ion_pulse_calibration();
        let sched = schedule(&bc, &tcal).unwrap();
        assert!(sched.has_no_overlaps());
        assert_eq!(sched.instructions.len(), 1);
        match sched.instructions[0] {
            PulseInstruction::Play { channel, start_time_ns, duration_ns, amplitude, .. } => {
                assert_eq!(channel, Channel::Control(0, 1));
                assert_eq!(start_time_ns, 0.0);
                assert_eq!(duration_ns, tcal.rzz.unwrap().duration_ns);
                assert_eq!(amplitude, tcal.rzz.unwrap().pi_amplitude, "Rzz(.., PI) should play at the full calibrated pi-pulse amplitude");
            }
            ref other => panic!("expected a Play instruction on Control(0,1), got {other:?}"),
        }
    }

    #[test]
    fn rzz_amplitude_scales_linearly_with_angle() {
        let bc = BackendCircuit {
            backend: Backend::TrappedIon,
            num_qubits: 2,
            num_clbits: 0,
            gates: vec![BackendGate::Rzz(0, 1, PI / 2.0)],
        };
        let tcal = trapped_ion_pulse_calibration();
        let sched = schedule(&bc, &tcal).unwrap();
        match sched.instructions[0] {
            PulseInstruction::Play { amplitude, .. } => {
                assert_eq!(
                    amplitude,
                    tcal.rzz.unwrap().pi_amplitude / 2.0,
                    "Rzz(.., PI/2) should play at half the calibrated pi-pulse amplitude"
                );
            }
            ref other => panic!("expected a Play instruction, got {other:?}"),
        }
    }

    #[test]
    fn rzz_occupies_both_qubits() {
        // Rzz(0,1) . Rot(0, PI): the Rot must not start until the Rzz
        // (which busies BOTH qubits) has ended -- same invariant as
        // entangling_gate_occupies_both_qubits, but for TrappedIon's
        // native two-qubit gate instead of Cx.
        let bc = BackendCircuit {
            backend: Backend::TrappedIon,
            num_qubits: 2,
            num_clbits: 0,
            gates: vec![BackendGate::Rzz(0, 1, PI), BackendGate::Rot(0, PI)],
        };
        let tcal = trapped_ion_pulse_calibration();
        let sched = schedule(&bc, &tcal).unwrap();
        assert!(sched.has_no_overlaps());
        let rot = sched
            .instructions
            .iter()
            .find(|i| matches!(i, PulseInstruction::Play { channel: Channel::Drive(0), .. }))
            .unwrap();
        assert_eq!(
            rot.start_time_ns(),
            tcal.rzz.unwrap().duration_ns,
            "Rot(0) must wait for the Rzz to finish on qubit 0: {rot:?}"
        );
    }

    #[test]
    fn schedules_a_real_trapped_ion_lowered_circuit_without_overlaps() {
        // Same end-to-end shape as the IbmQ/Rigetti tests above, but
        // through Backend::TrappedIon's Rzz-native path -- confirms
        // schedule's new Rzz handling actually holds up against the
        // real lowering pipeline, not just a hand-built BackendGate
        // vector.
        let mut c = Circuit::new(2);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 1))
            .push(Gate::Rz(1, 0.3))
            .push(Gate::Measure(0, 0))
            .push(Gate::Measure(1, 1));
        c.num_clbits = 2;

        let bc = backend::lower(&c, Backend::TrappedIon);
        assert!(
            bc.gates.iter().any(|g| matches!(g, BackendGate::Rzz(..))),
            "sanity check: TrappedIon's lowering should actually use Rzz, or this test \
             isn't exercising the path it claims to"
        );
        let sched = schedule(&bc, &trapped_ion_pulse_calibration()).unwrap();
        assert!(
            sched.has_no_overlaps(),
            "schedule for a real TrappedIon-lowered circuit must not overlap: {:?}",
            sched.instructions
        );
        assert!(sched.duration_ns() > 0.0);
    }

    #[test]
    fn ibm_calibration_rejects_a_rigetti_circuit_and_vice_versa() {
        // Uses the two *real* calibration tables against each other's
        // backend, not a synthetic mismatch -- confirms the guard
        // actually discriminates between the two real profiles this
        // module ships, not just an arbitrary placeholder.
        let ibm_bc = backend::lower(&Circuit::new(1), Backend::IbmQ);
        let rigetti_bc = backend::lower(&Circuit::new(1), Backend::Rigetti);

        assert!(schedule(&ibm_bc, &rigetti_ankaa3_pulse_calibration()).is_err());
        assert!(schedule(&rigetti_bc, &cal()).is_err());
        // And each backend's own calibration still works.
        assert!(schedule(&ibm_bc, &cal()).is_ok());
        assert!(schedule(&rigetti_bc, &rigetti_ankaa3_pulse_calibration()).is_ok());
    }

    #[test]
    fn if_is_scheduled_exactly_as_its_inner_gate_would_be() {
        // BackendGate::If(_, _, Rot(..)) should produce the identical
        // Play instruction (minus the classical condition, which this
        // static Schedule model doesn't represent at all -- see
        // schedule_one's own doc comment on that variant) that a bare
        // Rot would, at the same time, with the same effect on
        // busy_until. Built via backend::lower on an empty Circuit
        // (like this file's other tests) since BackendCircuit::new is
        // private outside backend.rs -- push is pub(crate), so the
        // gates themselves can still be added directly.
        let mut conditioned = backend::lower(&Circuit::new(1), Backend::TrappedIon);
        conditioned.push(BackendGate::If(0, true, Box::new(BackendGate::Rot(0, PI))));
        let mut plain = backend::lower(&Circuit::new(1), Backend::TrappedIon);
        plain.push(BackendGate::Rot(0, PI));

        let sched_conditioned = schedule(&conditioned, &trapped_ion_pulse_calibration()).unwrap();
        let sched_plain = schedule(&plain, &trapped_ion_pulse_calibration()).unwrap();
        assert_eq!(sched_conditioned.instructions, sched_plain.instructions);
    }

    #[test]
    fn if_wrapping_a_two_qubit_gate_still_advances_busy_until_on_both_wires() {
        // A conditioned two-qubit gate must still block a later
        // single-qubit pulse on either of its wires from starting
        // early -- same overlap-freedom guarantee as an unconditioned
        // two-qubit gate.
        let mut bc = backend::lower(&Circuit::new(2), Backend::TrappedIon);
        bc.push(BackendGate::If(0, true, Box::new(BackendGate::Rzz(0, 1, PI / 2.0))));
        bc.push(BackendGate::Rot(0, PI));
        let sched = schedule(&bc, &trapped_ion_pulse_calibration()).unwrap();
        assert!(
            sched.has_no_overlaps(),
            "a conditioned two-qubit gate must still reserve both its wires: {:?}",
            sched.instructions
        );
        assert_eq!(sched.instructions.len(), 2, "expected exactly the Rzz pulse plus the Rot pulse");
    }
}