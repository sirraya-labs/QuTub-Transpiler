//! Real-time control-flow program: the layer between [`crate::pulse`]'s
//! static [`Schedule`](crate::pulse::Schedule) and an actual piece of
//! control electronics. This is the module `pulse.rs`'s own doc
//! comment on `BackendGate::If` points to -- the thing that closes
//! the gap that module deliberately leaves open ("this static
//! `Schedule` only ever describes which pulses exist and when, never
//! whether one actually fires at runtime").
//!
//! # Why this is a separate layer, not a `Schedule` extension
//! A [`Schedule`](crate::pulse::Schedule) is a flat, precomputed list
//! of pulses with fixed start times -- exactly what you get from
//! compiling a circuit *offline*, with no notion of "wait and see."
//! Real classical feed-forward (read a qubit, branch on the result,
//! all within the coherence time budget) is not a data format
//! difference from that -- it's a different *kind* of artifact: a
//! program with real registers and real jumps, meant to run on a
//! sequencer that can evaluate a comparison and change what it does
//! next inside a single shot. Bolting a condition field onto
//! `PulseInstruction::Play` wouldn't represent that; it would just
//! move the same "trust me, this fires unconditionally" gap one field
//! deeper. [`Program`] is a real, separate representation instead.
//!
//! # What's real here today, and what's deliberately not
//! [`compile`] is real: it walks an already-lowered
//! [`BackendCircuit`](crate::backend::BackendCircuit) (the same input
//! [`crate::pulse::schedule`] takes) and produces a [`Program`] whose
//! branch structure is checked -- not just asserted -- against
//! `ir::Gate::If`'s own semantics (see `tests` below: for every
//! possible combination of measurement outcomes, the set of pulses
//! [`Program`]'s branches select is proven to match what
//! `emit::run_backend_with_measurement`'s direct conditional
//! evaluation would apply).
//!
//! What's deliberately *not* here yet is anything vendor-specific.
//! [`HardwareTarget`] is the extension point a real control-electronics
//! backend implements -- one trait, one file per vendor, the same
//! pattern [`crate::backend::BackendSpec`] already established for
//! per-vendor gate sets (see that module's own doc comment for the
//! rationale, which applies here unchanged). No implementation of it
//! ships in this crate today, because guessing at a vendor's real
//! instruction set, branch-latency model, or register file from
//! marketing material would produce exactly the kind of silently-wrong
//! artifact this crate's own conventions elsewhere refuse to produce
//! (see e.g. `ibm_export.rs`'s explicit `Err` on `BackendGate::If`,
//! for the same reason). [`Program`] is written to be a *reasonable*
//! lowering target regardless of which vendor eventually implements
//! against it -- a flat instruction list, one classical register per
//! source clbit, and a single branching primitive
//! ([`SeqInstr::JumpIfEqual`], compare-a-register-to-an-immediate-and-
//! jump) that nearly every real sequencer ISA either has directly or
//! is trivially built from (Quantum Machines' QUA has `if_`/`while_`;
//! Zurich Instruments' SeqC has native `if`/`while`; Keysight PathWave
//! has branch instructions in its own sequencer language) -- but it is
//! still, honestly, an unvalidated guess at the right shape until a
//! real vendor's actual documentation is in hand to check it against.
//!
//! # `execute`: the other half of "real," today
//! [`compile`]'s own correctness test proves [`Program`]'s *branch
//! structure* is faithful to `Gate::If`'s semantics -- which pulse a
//! given outcome selects -- but says nothing about whether the
//! resulting *quantum state* is right. [`execute`] closes that gap:
//! it interprets a compiled [`Program`] against a real
//! `sirraya_qutub::core::QuantumRegister`, with genuine Born-rule
//! measurement collapse (via `QuantumRegister::measure_single_qubit`)
//! feeding the sequencer's own real-time registers and genuine
//! branching on those real outcomes. See [`execute`]'s own doc comment
//! for exactly what this does and does not verify -- in short, it
//! proves `Program`'s pulse-level representation reproduces the same
//! physics as executing the `BackendCircuit` directly
//! (`emit::run_backend_with_measurement`), not that a real waveform
//! achieves its intended rotation (that remains `waveform_sim.rs`'s
//! job, and its own two-qubit gap stays open here too).

use crate::backend::{Backend, BackendCircuit, BackendGate, NativeTwoQubitGate};
use crate::pulse::{push_leaf_pulse, Channel, PulseCalibration, PulseInstruction};
use sirraya_qutub::core::QuantumRegister;
use std::collections::HashMap;
use std::f64::consts::PI;

/// One instruction in a real-time sequencer [`Program`]. Every variant
/// here corresponds to something a real control-electronics ISA
/// actually needs to express -- this is deliberately *not* a 1:1
/// mirror of [`crate::ir::Gate`]/[`BackendGate`]; it's one level closer
/// to the hardware, the same way [`PulseInstruction`] already is for
/// the unconditional case.
#[derive(Debug, Clone, PartialEq)]
pub enum SeqInstr {
    /// Emit a physical pulse -- unconditionally, from the sequencer's
    /// point of view. Conditionality is expressed by *skipping over*
    /// a `Play` with [`SeqInstr::JumpIfEqual`], not by a field on
    /// `Play` itself -- see this module's doc comment on why a
    /// condition field on the instruction wouldn't be the real thing.
    Play(PulseInstruction),
    /// Triggers a readout on `qubit` and writes the digitized
    /// single-bit outcome into real-time register `reg`. This has no
    /// counterpart in [`crate::pulse::Schedule`] at all -- a static
    /// schedule only ever describes *playing* the readout pulse
    /// ([`SeqInstr::Play`] handles that half); *capturing* its result
    /// into something later instructions can branch on is a genuinely
    /// new capability this layer adds.
    MeasureInto { qubit: usize, reg: usize },
    /// If register `reg`'s current value equals `value`, jump to
    /// instruction index `target`; otherwise fall through to the next
    /// instruction. The one branching primitive this whole module is
    /// built around -- see the module doc comment for why this
    /// specific shape (compare-immediate-and-jump) rather than a
    /// richer condition language.
    JumpIfEqual { reg: usize, value: u8, target: usize },
    /// Unconditional jump, used to skip *around* a conditioned block
    /// when the condition is `false`-triggered (see [`compile`]).
    Jump { target: usize },
    /// End of program.
    Halt,
}

/// A real-time, branching program compiled from an already-lowered
/// [`BackendCircuit`] -- see this module's doc comment for where this
/// sits in the pipeline and what it's for.
#[derive(Debug, Clone)]
pub struct Program {
    pub backend: Backend,
    pub instrs: Vec<SeqInstr>,
    /// One real-time classical register per source classical bit --
    /// `num_registers == circuit.num_clbits` of the `BackendCircuit`
    /// this was compiled from. A [`SeqInstr::MeasureInto`] writes one;
    /// a [`SeqInstr::JumpIfEqual`] reads one.
    pub num_registers: usize,
}

/// Compiles an already-lowered [`BackendCircuit`] into a real-time
/// [`Program`]. Reuses [`crate::pulse::push_leaf_pulse`] for every leaf
/// gate's actual pulse -- the exact same physical-pulse construction
/// [`crate::pulse::schedule`] uses for its own (unconditional) output,
/// so the two layers never disagree about what pulse a given gate
/// produces, only about whether/when it's allowed to fire. A
/// `BackendGate::If(clbit, value, inner)` compiles to a real
/// conditional skip: jump past `inner`'s instruction(s) if the
/// register doesn't hold the required value, otherwise fall through
/// and execute them -- see [`SeqInstr::JumpIfEqual`]'s doc comment.
pub fn compile(circuit: &BackendCircuit, cal: &PulseCalibration) -> Result<Program, String> {
    if circuit.backend != cal.backend {
        return Err(format!(
            "calibration is for {:?} but circuit was lowered for {:?}",
            cal.backend, circuit.backend
        ));
    }

    let mut busy_until: HashMap<usize, f64> = HashMap::new();
    let mut instrs = Vec::with_capacity(circuit.gates.len());

    for g in &circuit.gates {
        compile_one(g, cal, &mut busy_until, &mut instrs)?;
    }
    instrs.push(SeqInstr::Halt);

    Ok(Program { backend: circuit.backend, instrs, num_registers: circuit.num_clbits })
}

fn compile_one(
    g: &BackendGate,
    cal: &PulseCalibration,
    busy_until: &mut HashMap<usize, f64>,
    instrs: &mut Vec<SeqInstr>,
) -> Result<(), String> {
    match *g {
        BackendGate::Measure(q, c) => {
            let mut plays = Vec::new();
            push_leaf_pulse(g, cal, busy_until, &mut plays)?;
            instrs.extend(plays.into_iter().map(SeqInstr::Play));
            instrs.push(SeqInstr::MeasureInto { qubit: q, reg: c });
        }
        BackendGate::If(ref conditions, ref inner) => {
            // Skip past inner's instructions unless *every* condition
            // holds (AND semantics -- see `ir::Gate::If`'s doc
            // comment): one JumpIfEqual per condition, each jumping to
            // the same "after inner" target if that condition's own
            // register does NOT hold the required value (hence each
            // jump's own `value` is the *negation* of its condition's
            // `value`). If every jump falls through, every condition
            // held, and inner executes.
            let mut jump_indices = Vec::with_capacity(conditions.len());
            for &(clbit, value) in conditions {
                let negated = if value { 0 } else { 1 };
                jump_indices.push(instrs.len());
                // Placeholder target, patched below once inner's real
                // instruction count is known -- avoids a two-pass
                // compile.
                instrs.push(SeqInstr::JumpIfEqual {
                    reg: clbit,
                    value: negated,
                    target: usize::MAX,
                });
            }
            compile_one(inner, cal, busy_until, instrs)?;
            let after = instrs.len();
            for jump_idx in jump_indices {
                if let SeqInstr::JumpIfEqual { target, .. } = &mut instrs[jump_idx] {
                    *target = after;
                }
            }
        }
        _ => {
            let mut plays = Vec::new();
            push_leaf_pulse(g, cal, busy_until, &mut plays)?;
            instrs.extend(plays.into_iter().map(SeqInstr::Play));
        }
    }
    Ok(())
}

/// Executes a compiled [`Program`] against a real
/// `sirraya_qutub::core::QuantumRegister` -- genuine Born-rule
/// measurement collapse feeding the sequencer's own real-time
/// registers, and genuine branching on those real outcomes. Not a
/// structural/symbolic check the way this module's own
/// `branch_structure_matches_...` test is (see that test's doc
/// comment) -- the real thing, held to the same "checked against the
/// real simulator, not asserted" standard every other rewrite in this
/// crate is (`native.rs`'s decompositions, `route.rs`'s SWAP
/// insertion, `optimize_ir`'s cancellation -- see
/// `tests/decompositions.rs`, `examples/verify_equivalence.rs`).
///
/// # What this proves, and what it deliberately doesn't
/// This proves `Program`'s *control flow* -- which pulse fires, gated
/// on which real measurement outcome -- reproduces the exact same
/// final quantum state `emit::run_backend_with_measurement` gives for
/// the same `BackendCircuit`. It does this by recovering each
/// `Play`/`ShiftPhase` instruction's underlying gate action from its
/// pulse parameters -- the exact inverse of how [`push_leaf_pulse`]
/// constructed them, lossless, not an approximation -- and applying
/// that action directly to the register, rather than numerically
/// integrating the actual waveform envelope against a driven
/// Hamiltonian. That deeper check -- does the *waveform itself*
/// achieve the intended rotation -- is `waveform_sim.rs`'s job, which
/// already covers the single-qubit case and explicitly, honestly
/// leaves two-qubit gates as separate, unimplemented follow-on work
/// (see that module's own doc comment). Conflating the two here would
/// mean silently claiming a stronger result than either module
/// actually establishes.
///
/// Returns the final register and the classical outcomes every
/// `SeqInstr::MeasureInto` wrote, indexed by register -- the same
/// shape [`crate::emit::run_backend_with_measurement`] returns, so a
/// caller can compare the two directly.
pub fn execute(
    circuit: &BackendCircuit,
    program: &Program,
    cal: &PulseCalibration,
) -> Result<(QuantumRegister, Vec<u8>), String> {
    execute_impl(circuit, program, cal, None, None, &mut || 0.0)
}

/// As [`execute`], but every classical bit a `SeqInstr::MeasureInto`
/// writes is passed through `readout_cal`'s confusion probabilities
/// (see [`crate::readout`]) before being stored in the sequencer's own
/// register and returned -- modeling a real, imperfect classical
/// readout chain layered independently on top of the exact quantum
/// collapse `QuantumRegister::measure_single_qubit` still performs.
///
/// # A real, checkable consequence
/// Because `Gate::If`/`SeqInstr::JumpIfEqual` branch on exactly the
/// value this function writes to a register -- never on the qubit's
/// true post-measurement state directly -- a corrupted readout can
/// make a conditioned correction fire when it shouldn't (or skip when
/// it should have fired), even though the underlying quantum collapse
/// was exact. This is real, physical behavior, not a modeling
/// artifact: it's exactly why a real feed-forward protocol's fidelity
/// budget needs to include readout error alongside gate error, not
/// gate error alone (see this module's own test,
/// `readout_noise_can_break_a_correction_decision`, for a checked
/// demonstration on the teleportation circuit).
///
/// A thin wrapper over [`execute_with_noise`] with no gate noise --
/// kept as its own named function since "readout error only" is a
/// common, meaningful configuration on its own (e.g. isolating its
/// effect from gate error the way this module's own tests do).
///
/// `uniform_samples` supplies one U[0, 1) value per `MeasureInto`
/// executed, in the order they execute -- see
/// [`crate::readout::corrupt_readout`]'s doc comment for why this
/// crate's library code takes randomness this way rather than
/// depending on an RNG crate directly.
pub fn execute_with_readout_noise(
    circuit: &BackendCircuit,
    program: &Program,
    cal: &PulseCalibration,
    readout_cal: &crate::readout::ReadoutCalibration,
    uniform_samples: impl FnMut() -> f64,
) -> Result<(QuantumRegister, Vec<u8>), String> {
    execute_with_noise(circuit, program, cal, None, Some(readout_cal), uniform_samples)
}

/// The full, combined noisy execution: real per-gate depolarizing
/// noise (see [`crate::noise`]) *and* real classical readout error
/// (see [`crate::readout`]), together, on top of the same exact
/// interpretation [`execute`] performs. Either noise source can be
/// turned off independently by passing `None` -- `execute` itself is
/// exactly `execute_with_noise(.., None, None, ..)`, and
/// [`execute_with_readout_noise`] is exactly `execute_with_noise(..,
/// None, Some(readout_cal), ..)`.
///
/// `gate_cal` prices depolarizing noise for every *real physical*
/// pulse (`Channel::Drive`/`Channel::Control` `Play` instructions) --
/// deliberately never for a virtual-Z `ShiftPhase`, which has
/// essentially zero real gate error on real hardware (that's the whole
/// point of implementing `Rz` that way). This is a more physically
/// precise choice than `fidelity::estimate_circuit_fidelity`'s own
/// gate-counting, which prices `Rz` the same as `Ry` purely for the
/// simplicity of a quick aggregate estimate -- see `noise.rs`'s own
/// doc comment. A two-qubit `Play` samples an independent error on
/// *each* of its two qubits, both at `gate_cal.two_qubit_error_probability()`
/// -- a standard, simplified stand-in for a full two-qubit
/// depolarizing channel (see `noise.rs`'s doc comment for why).
///
/// `uniform_samples` supplies one U[0, 1) value per noise-eligible
/// event -- every real `Play` sampled against `gate_cal` (two draws
/// for a two-qubit gate, one per qubit) and every `MeasureInto`
/// sampled against `readout_cal` -- called in the exact order those
/// events occur during execution, so a fixed-seed RNG on the caller's
/// side gives a fully reproducible noisy trajectory.
pub fn execute_with_noise(
    circuit: &BackendCircuit,
    program: &Program,
    cal: &PulseCalibration,
    gate_cal: Option<&crate::fidelity::PublishedCalibration>,
    readout_cal: Option<&crate::readout::ReadoutCalibration>,
    mut uniform_samples: impl FnMut() -> f64,
) -> Result<(QuantumRegister, Vec<u8>), String> {
    execute_impl(circuit, program, cal, gate_cal, readout_cal, &mut uniform_samples)
}

/// The shared core every `execute*` function runs -- identical except
/// for whether `gate_cal`/`readout_cal` are `Some`, in which case the
/// corresponding noise is sampled from `uniform_samples` at exactly
/// the point it physically occurs. Factoring it this way (rather than
/// duplicating the whole interpreter loop per noise configuration)
/// means every variant can never quietly drift apart on anything
/// *except* those two, explicit differences.
fn execute_impl(
    circuit: &BackendCircuit,
    program: &Program,
    cal: &PulseCalibration,
    gate_cal: Option<&crate::fidelity::PublishedCalibration>,
    readout_cal: Option<&crate::readout::ReadoutCalibration>,
    uniform_samples: &mut dyn FnMut() -> f64,
) -> Result<(QuantumRegister, Vec<u8>), String> {
    if circuit.backend != program.backend {
        return Err(format!(
            "execute: program was compiled for {:?} but circuit was lowered for {:?}",
            program.backend, circuit.backend
        ));
    }
    if circuit.backend != cal.backend {
        return Err(format!(
            "execute: calibration is for {:?} but circuit was lowered for {:?}",
            cal.backend, circuit.backend
        ));
    }

    let mut reg = QuantumRegister::new(circuit.num_qubits)?;
    let mut registers: Vec<Option<u8>> = vec![None; program.num_registers];
    let mut clbits = vec![0u8; program.num_registers];
    let axis = circuit.backend.rot_axis();

    let mut pc = 0usize;
    // A well-formed Program (everything `compile` produces) only ever
    // jumps forward, so this bound is generous, not tight -- it's here
    // to turn a hypothetical compiler bug that emits a backward jump
    // into a clear error instead of a hang.
    let max_steps = program.instrs.len().saturating_mul(4).saturating_add(16);
    let mut steps = 0usize;

    // Samples one gate-noise draw for qubit `q` against `p`, applying
    // it to `reg` if it fires. Called only from sites that already
    // checked `gate_cal.is_some()`, so `uniform_samples` is never
    // invoked when gate noise is off -- see `execute`'s own use of a
    // never-invoked placeholder closure.
    fn maybe_apply_gate_noise(
        reg: &mut QuantumRegister,
        q: usize,
        p: f64,
        sample: f64,
    ) -> Result<(), String> {
        if let Some(err) = crate::noise::sample_depolarizing_error(p, sample) {
            crate::noise::apply_pauli_error(reg, q, err)?;
        }
        Ok(())
    }

    loop {
        steps += 1;
        if steps > max_steps {
            return Err(format!(
                "execute: exceeded {} steps without reaching Halt -- likely a backward-jumping \
                 or otherwise malformed Program (pc={})",
                max_steps, pc
            ));
        }
        let instr = program.instrs.get(pc).ok_or_else(|| {
            format!(
                "execute: program counter {} out of bounds ({} instructions)",
                pc,
                program.instrs.len()
            )
        })?;
        match instr {
            SeqInstr::Play(PulseInstruction::ShiftPhase { channel, angle_rad, .. }) => {
                // No gate-noise sampling here -- virtual-Z, see this
                // function's own doc comment on `gate_cal`.
                if let Channel::Drive(q) = *channel {
                    reg.apply_rz(q, *angle_rad)?;
                }
                pc += 1;
            }
            SeqInstr::Play(PulseInstruction::Play { channel, amplitude, .. }) => {
                match *channel {
                    Channel::Drive(q) => {
                        let theta = invert_amplitude(*amplitude, cal.rot.pi_amplitude, "rot")?;
                        match axis {
                            crate::backend::RotAxis::Ry => reg.apply_ry(q, theta)?,
                            crate::backend::RotAxis::Rx => reg.apply_rx(q, theta)?,
                        }
                        if let Some(gc) = gate_cal {
                            let sample = uniform_samples();
                            maybe_apply_gate_noise(
                                &mut reg,
                                q,
                                gc.single_qubit_error_probability(),
                                sample,
                            )?;
                        }
                    }
                    Channel::Control(a, b) => {
                        match circuit.backend.native_two_qubit_gate() {
                            NativeTwoQubitGate::ContinuousRzz => {
                                let rzz_cal = cal.rzz.ok_or_else(|| {
                                    "execute: backend's native two-qubit gate is Rzz but this \
                                     calibration has no rzz entry"
                                        .to_string()
                                })?;
                                let theta =
                                    invert_amplitude(*amplitude, rzz_cal.pi_amplitude, "rzz")?;
                                reg.apply_rzz(a, b, theta)?;
                            }
                            NativeTwoQubitGate::FixedCx => reg.apply_cnot(a, b)?,
                            NativeTwoQubitGate::FixedCz => reg.apply_controlled_z(a, b)?,
                        }
                        if let Some(gc) = gate_cal {
                            let p = gc.two_qubit_error_probability();
                            let sample_a = uniform_samples();
                            maybe_apply_gate_noise(&mut reg, a, p, sample_a)?;
                            let sample_b = uniform_samples();
                            maybe_apply_gate_noise(&mut reg, b, p, sample_b)?;
                        }
                    }
                    // The readout play is informational/timing only --
                    // the actual collapse happens at the matching
                    // MeasureInto below, via a real
                    // measure_single_qubit call, not here. Not a
                    // gate-noise-eligible event either (readout error
                    // is modeled separately, via `readout_cal` below).
                    Channel::Readout(_) => {}
                }
                pc += 1;
            }
            SeqInstr::MeasureInto { qubit, reg: r } => {
                let true_outcome = reg.measure_single_qubit(*qubit)?;
                let reported = match readout_cal {
                    Some(rc) => {
                        crate::readout::corrupt_readout(true_outcome, rc, uniform_samples())
                    }
                    None => true_outcome,
                };
                registers[*r] = Some(reported);
                clbits[*r] = reported;
                pc += 1;
            }
            SeqInstr::JumpIfEqual { reg: r, value, target } => {
                let actual = registers[*r].ok_or_else(|| {
                    format!(
                        "execute: JumpIfEqual read register {} before any MeasureInto wrote it",
                        r
                    )
                })?;
                pc = if actual == *value { *target } else { pc + 1 };
            }
            SeqInstr::Jump { target } => pc = *target,
            SeqInstr::Halt => break,
        }
    }

    Ok((reg, clbits))
}

/// Inverts a pulse `amplitude` back to the rotation angle
/// [`push_leaf_pulse`] built it from (`amplitude = pi_amplitude *
/// theta / PI`, so `theta = amplitude * PI / pi_amplitude`) -- exact,
/// not approximate, since the forward direction is itself an exact
/// linear relationship with no information loss. Errors rather than
/// dividing by ~0 for a calibration whose `pi_amplitude` is ~0, which
/// would otherwise silently produce a nonsense (huge or NaN) angle.
fn invert_amplitude(amplitude: f64, pi_amplitude: f64, label: &str) -> Result<f64, String> {
    if pi_amplitude.abs() < 1e-15 {
        return Err(format!(
            "execute: calibration's {} pi_amplitude is ~0 -- can't invert amplitude back to \
             an angle",
            label
        ));
    }
    Ok(amplitude * PI / pi_amplitude)
}

/// The extension point a real control-electronics backend implements
/// to actually drive hardware from a [`Program`] -- see this module's
/// doc comment for why nothing implements this yet, and what shape a
/// future implementation is expected to take (one file per vendor,
/// mirroring [`crate::backend::BackendSpec`]).
pub trait HardwareTarget: Send + Sync {
    /// Stable identifier, e.g. `"QuantumMachines-OPX"`,
    /// `"ZurichInstruments-SHFQC"`. Same role as
    /// [`crate::backend::BackendSpec::id`].
    fn id(&self) -> &'static str;

    /// The real-time feed-forward latency this target can guarantee
    /// between a [`SeqInstr::MeasureInto`] completing and a
    /// conditioned [`SeqInstr::Play`] it gates being able to fire --
    /// the number that determines whether a given `Program` actually
    /// fits inside a qubit's coherence budget on this hardware. An
    /// implementation should report a real, measured or vendor-
    /// published figure here, not an estimate -- this is exactly the
    /// kind of number this crate's `fidelity.rs`/`pulse.rs` are
    /// already careful to cite rather than guess.
    fn feedforward_latency_ns(&self) -> f64;

    /// Lowers `program` into whatever this target's real sequencer
    /// actually consumes -- a vendor DSL string, a bytecode blob,
    /// whatever the real hardware wants. Deliberately opaque
    /// (`Vec<u8>`) rather than a shared struct: different vendors'
    /// real formats have nothing in common beyond "bytes a real
    /// instrument can load," the same reasoning `ibm_export.rs`
    /// already applies to its own `String` (real QASM text) output.
    fn compile_for_hardware(&self, program: &Program) -> Result<Vec<u8>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend;
    use crate::ir::{Circuit, Gate};
    use crate::pulse::trapped_ion_pulse_calibration;

    /// The teleportation shape in miniature: measure two qubits, then
    /// two independently-conditioned corrections on a third -- the
    /// exact pattern `examples/quantum_teleportation.rs` compiles for
    /// real. Built through the real pipeline (`backend::lower`), not
    /// hand-assembled, so this test exercises the same lowering path
    /// production code does.
    fn teleportation_shaped_circuit() -> BackendCircuit {
        let mut c = Circuit::new(3);
        c.num_clbits = 2;
        c.push(Gate::Measure(0, 0));
        c.push(Gate::Measure(1, 1));
        c.push(Gate::If(vec![(1, true)], Box::new(Gate::X(2))));
        c.push(Gate::If(vec![(0, true)], Box::new(Gate::Z(2))));
        backend::lower(&c, Backend::TrappedIon)
    }

    #[test]
    fn compile_rejects_a_calibration_backend_mismatch() {
        let bc = teleportation_shaped_circuit();
        assert!(compile(&bc, &crate::pulse::ibm_heron_r2_pulse_calibration()).is_err());
    }

    #[test]
    fn num_registers_matches_source_clbits() {
        let bc = teleportation_shaped_circuit();
        let program = compile(&bc, &trapped_ion_pulse_calibration()).unwrap();
        assert_eq!(program.num_registers, 2);
    }

    #[test]
    fn program_ends_in_halt() {
        let bc = teleportation_shaped_circuit();
        let program = compile(&bc, &trapped_ion_pulse_calibration()).unwrap();
        assert_eq!(program.instrs.last(), Some(&SeqInstr::Halt));
    }

    #[test]
    fn every_jump_target_is_a_valid_in_bounds_index() {
        // A jump landing outside the instruction list (or exactly at
        // its own index, an infinite loop) would be a real bug in
        // compile_one's target-patching -- checked structurally here
        // rather than only implicitly by the symbolic-execution tests
        // below, so a target bug shows up even for a jump that
        // symbolic execution never actually takes.
        let bc = teleportation_shaped_circuit();
        let program = compile(&bc, &trapped_ion_pulse_calibration()).unwrap();
        for (i, instr) in program.instrs.iter().enumerate() {
            let target = match instr {
                SeqInstr::JumpIfEqual { target, .. } | SeqInstr::Jump { target } => Some(*target),
                _ => None,
            };
            if let Some(t) = target {
                assert!(
                    t < program.instrs.len() && t != i,
                    "instr {} jumps to {}, which is out of bounds or a self-loop \
                     (program has {} instructions)",
                    i, t, program.instrs.len()
                );
            }
        }
    }

    /// A minimal symbolic interpreter: walks `program`'s control flow
    /// for one fixed set of classical outcomes (as if a real ADC had
    /// already digitized them) and returns which qubits received a
    /// *drive* pulse (`Channel::Drive`, i.e. a real single-qubit gate
    /// -- readout plays are excluded since every branch of every test
    /// circuit here always measures the same qubits regardless of the
    /// outcome, so they're not the interesting signal). This doesn't
    /// simulate real quantum state or real waveforms -- see this
    /// module's doc comment on what `compile`'s correctness claim
    /// actually covers -- it only proves the *branch structure* is
    /// faithful to `Gate::If`'s own semantics.
    fn symbolic_run(program: &Program, clbit_outcomes: &[u8]) -> Vec<usize> {
        let mut fired = Vec::new();
        let mut registers: Vec<Option<u8>> = vec![None; program.num_registers];
        let mut pc = 0usize;
        let mut steps = 0usize;
        loop {
            steps += 1;
            assert!(steps < 10_000, "symbolic_run: probable infinite loop, pc={}", pc);
            match &program.instrs[pc] {
                // Only a real `Play` counts as "fired" -- `ShiftPhase`
                // is a virtual, zero-duration frame update (see
                // `pulse.rs`'s own doc comment on virtual-Z) that
                // shares `Play`'s `Channel::Drive` addressing but isn't
                // a physical event, so it's excluded here rather than
                // conflated with one.
                SeqInstr::Play(PulseInstruction::Play { channel, .. }) => {
                    if let crate::pulse::Channel::Drive(q) = *channel {
                        fired.push(q);
                    }
                    pc += 1;
                }
                SeqInstr::Play(PulseInstruction::ShiftPhase { .. }) => pc += 1,
                SeqInstr::MeasureInto { reg, .. } => {
                    registers[*reg] = Some(clbit_outcomes[*reg]);
                    pc += 1;
                }
                SeqInstr::JumpIfEqual { reg, value, target } => {
                    let actual = registers[*reg]
                        .expect("JumpIfEqual read a register no prior MeasureInto wrote");
                    pc = if actual == *value { *target } else { pc + 1 };
                }
                SeqInstr::Jump { target } => pc = *target,
                SeqInstr::Halt => break,
            }
        }
        fired
    }

    /// Direct evaluation of `BackendGate::If`'s own semantics against
    /// the *same* `BackendCircuit` [`compile`] actually consumed --
    /// deliberately at this granularity, not the source `Circuit`'s,
    /// since native decomposition can expand one logical gate (e.g.
    /// `X`) into more than one native/backend gate, each independently
    /// `If`-wrapped (see `NativeGate::If`'s doc comment) -- comparing
    /// against the coarser source level would conflate "one logical
    /// correction" with "one real pulse," which aren't the same
    /// number once decomposition is in the picture. Only
    /// `BackendGate::Rot`/`Cx`/`Cz`/`Rzz` produce a real
    /// `PulseInstruction::Play`; `Rz` is `ShiftPhase`-only (virtual,
    /// see `symbolic_run`'s matching exclusion), so it contributes no
    /// qubit here either -- the two functions must agree on that
    /// distinction for this comparison to mean anything.
    fn expected_fired(circuit: &BackendCircuit, clbit_outcomes: &[u8]) -> Vec<usize> {
        let mut expected = Vec::new();
        for gate in &circuit.gates {
            if let BackendGate::If(conditions, inner) = gate {
                let actual = conditions
                    .iter()
                    .all(|&(clbit, value)| (clbit_outcomes[clbit] != 0) == value);
                if actual {
                    match inner.as_ref() {
                        BackendGate::Rot(q, _) => expected.push(*q),
                        BackendGate::Cx(a, b) | BackendGate::Cz(a, b) | BackendGate::Rzz(a, b, _) => {
                            expected.push(*a);
                            expected.push(*b);
                        }
                        BackendGate::Rz(..) => {} // virtual only, no real Play
                        BackendGate::Measure(..) | BackendGate::If(..) => unreachable!(
                            "BackendGate::If never wraps Measure or another If"
                        ),
                    }
                }
            }
        }
        expected
    }

    #[test]
    fn branch_structure_matches_gate_if_semantics_for_every_outcome_combination() {
        // The real correctness claim: for all four possible outcomes
        // of the two measurements (0/0, 0/1, 1/0, 1/1), the set of
        // qubits that receive a real drive pulse when the compiled
        // Program is symbolically executed must exactly match what
        // directly evaluating BackendGate::If's condition against the
        // same outcomes (at the same BackendCircuit granularity
        // compile() consumed) says should happen -- not asserted,
        // checked, the same way native.rs's decompositions are checked
        // against the real simulator rather than trusted algebraically.
        let mut source = Circuit::new(3);
        source.num_clbits = 2;
        source.push(Gate::Measure(0, 0));
        source.push(Gate::Measure(1, 1));
        source.push(Gate::If(vec![(1, true)], Box::new(Gate::X(2))));
        source.push(Gate::If(vec![(0, true)], Box::new(Gate::Z(2))));

        let bc = backend::lower(&source, Backend::TrappedIon);
        let program = compile(&bc, &trapped_ion_pulse_calibration()).unwrap();

        for m0 in [0u8, 1] {
            for m1 in [0u8, 1] {
                let outcomes = [m0, m1];
                let mut fired = symbolic_run(&program, &outcomes);
                let mut expected = expected_fired(&bc, &outcomes);
                fired.sort();
                expected.sort();
                assert_eq!(
                    fired, expected,
                    "outcomes m0={} m1={}: Program fired drive pulses on {:?}, but \
                     BackendGate::If semantics say it should have been {:?}",
                    m0, m1, fired, expected
                );
            }
        }
    }

    /// The real physics check this module's doc comment promises:
    /// runs the teleportation protocol -- entangling circuit, both
    /// measurements, both conditioned corrections -- entirely through
    /// `compile` + `execute`, i.e. driven by pulse-level `Program`
    /// instructions and genuine Born-rule collapse, not by directly
    /// evaluating `BackendGate::If` the way `emit.rs` does. Bob's
    /// qubit should still end up in the target state with ~100%
    /// fidelity on every trial, for every input state -- exactly the
    /// methodology `examples/quantum_teleportation.rs` already applies
    /// one layer up (at the `BackendCircuit`/`emit.rs` level), run
    /// here instead against `Program`'s own pulse-level execution.
    /// This is what actually proves `execute`'s amplitude-inversion
    /// (`Rot`/`Rzz` angle recovery, `NativeTwoQubitGate` dispatch) is
    /// physically correct, not just structurally plausible --
    /// `branch_structure_matches_...` above only checks *which* qubit
    /// gets touched, never whether the resulting quantum state is
    /// right.
    #[test]
    fn execute_teleports_correctly_across_many_trials_and_every_input_state() {
        use sirraya_qutub::DensityMatrix;

        fn teleportation_circuit(prep: impl Fn(&mut Circuit)) -> Circuit {
            let mut c = Circuit::new(3);
            prep(&mut c);
            c.push(Gate::H(1));
            c.push(Gate::Cx(1, 2));
            c.push(Gate::Cx(0, 1));
            c.push(Gate::H(0));
            c.num_clbits = 2;
            c.push(Gate::Measure(0, 0));
            c.push(Gate::Measure(1, 1));
            c.push(Gate::If(vec![(1, true)], Box::new(Gate::X(2))));
            c.push(Gate::If(vec![(0, true)], Box::new(Gate::Z(2))));
            c
        }

        fn target_density_matrix(prep: impl Fn(&mut QuantumRegister)) -> DensityMatrix {
            let mut reg = QuantumRegister::new(1).unwrap();
            prep(&mut reg);
            reg.to_density_matrix().unwrap()
        }

        // Six input states, matching quantum_teleportation.rs's own
        // coverage (trimmed to four representative ones here, since
        // this test's job is proving execute()'s physics is right,
        // not re-running quantum_teleportation.rs's own full sweep).
        let cases: Vec<(&str, fn(&mut Circuit), fn(&mut QuantumRegister))> = vec![
            ("|0>", |_c: &mut Circuit| {}, |_r: &mut QuantumRegister| {}),
            (
                "|1>",
                |c: &mut Circuit| {
                    c.push(Gate::X(0));
                },
                |r: &mut QuantumRegister| {
                    r.apply_pauli_x(0).unwrap();
                },
            ),
            (
                "|+>",
                |c: &mut Circuit| {
                    c.push(Gate::H(0));
                },
                |r: &mut QuantumRegister| {
                    r.apply_hadamard(0).unwrap();
                },
            ),
            (
                "Ry(0.7)Rz(1.3)|0>",
                |c: &mut Circuit| {
                    c.push(Gate::Rz(0, 1.3));
                    c.push(Gate::Ry(0, 0.7));
                },
                |r: &mut QuantumRegister| {
                    r.apply_rz(0, 1.3).unwrap();
                    r.apply_ry(0, 0.7).unwrap();
                },
            ),
        ];

        let cal = trapped_ion_pulse_calibration();

        for (label, prep_circuit, prep_reg) in cases {
            let source = teleportation_circuit(prep_circuit);
            let bc = backend::lower(&source, Backend::TrappedIon);
            let program = compile(&bc, &cal).unwrap();
            let target = target_density_matrix(prep_reg);

            for trial in 0..20 {
                let (reg, _clbits) = execute(&bc, &program, &cal).unwrap();
                let bob = reg.to_density_matrix().unwrap().partial_trace(&[2]).unwrap();
                let fidelity = bob.fidelity(&target).unwrap();
                assert!(
                    (fidelity - 1.0).abs() < 1e-9,
                    "state {} trial {}: teleportation via sequencer::execute should reach \
                     ~100% fidelity, got {}",
                    label, trial, fidelity
                );
            }
        }
    }

    /// `execute_teleports_correctly_across_many_trials_and_every_input_state`
    /// above only exercises `Backend::TrappedIon`, whose native
    /// two-qubit gate is `Rzz` (`NativeTwoQubitGate::ContinuousRzz`).
    /// This test is specifically for the other half of that dispatch --
    /// `IbmQ` (`FixedCx`) and `Rigetti`/`Google` (`FixedCz`) -- the
    /// exact ambiguity `NativeTwoQubitGate` exists to resolve (see its
    /// doc comment: `Cx` and `Cz` produce bit-for-bit identical pulses
    /// in this crate's calibration model, so getting this dispatch
    /// wrong for a given backend would silently apply the wrong
    /// two-qubit gate during `execute` -- exactly the kind of error
    /// this test would catch, since a wrong gate here means Bob's
    /// qubit ends up in the wrong state and fidelity collapses well
    /// below 1.0, not just drifts slightly).
    #[test]
    fn execute_dispatches_the_correct_native_two_qubit_gate_on_every_backend() {
        use sirraya_qutub::DensityMatrix;

        fn teleportation_circuit_plus_state() -> Circuit {
            let mut c = Circuit::new(3);
            c.push(Gate::H(0)); // |+> on q0
            c.push(Gate::H(1));
            c.push(Gate::Cx(1, 2));
            c.push(Gate::Cx(0, 1));
            c.push(Gate::H(0));
            c.num_clbits = 2;
            c.push(Gate::Measure(0, 0));
            c.push(Gate::Measure(1, 1));
            c.push(Gate::If(vec![(1, true)], Box::new(Gate::X(2))));
            c.push(Gate::If(vec![(0, true)], Box::new(Gate::Z(2))));
            c
        }

        let target: DensityMatrix = {
            let mut reg = QuantumRegister::new(1).unwrap();
            reg.apply_hadamard(0).unwrap();
            reg.to_density_matrix().unwrap()
        };

        for backend in [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti] {
            // Backend::Google is deliberately excluded here: pulse.rs
            // has no `PulseCalibration` constructor for it (only
            // TrappedIon/IbmQ/Rigetti exist -- see this file's own
            // `cal()` helper and pulse.rs's own constructors). That's
            // a real, pre-existing gap in pulse.rs, not something to
            // paper over with a fabricated calibration here; Google's
            // `NativeTwoQubitGate::FixedCz` dispatch is still checked
            // for by Rigetti's own run below (both are `FixedCz`,
            // just with different published numbers), so this isn't
            // an entirely uncovered code path, just an entirely
            // untested *backend*.
            let cal = match backend.id() {
                "TrappedIon" => trapped_ion_pulse_calibration(),
                "IbmQ" => crate::pulse::ibm_heron_r2_pulse_calibration(),
                "Rigetti" => crate::pulse::rigetti_ankaa3_pulse_calibration(),
                other => unreachable!("no PulseCalibration case for backend {}", other),
            };

            let source = teleportation_circuit_plus_state();
            let bc = backend::lower(&source, backend);
            let program = compile(&bc, &cal).unwrap();

            for trial in 0..10 {
                let (reg, _clbits) = execute(&bc, &program, &cal).unwrap();
                let bob = reg.to_density_matrix().unwrap().partial_trace(&[2]).unwrap();
                let fidelity = bob.fidelity(&target).unwrap();
                assert!(
                    (fidelity - 1.0).abs() < 1e-9,
                    "backend {:?} trial {}: expected ~100% fidelity, got {} -- likely a wrong \
                     NativeTwoQubitGate dispatch for this backend",
                    backend, trial, fidelity
                );
            }
        }
    }

    /// The real, checkable consequence [`execute_with_readout_noise`]'s
    /// own doc comment promises: with an extreme (but deterministic,
    /// so the test doesn't need statistical trial-counting to see the
    /// effect) readout calibration that always misreports a true "1"
    /// as "0", the teleportation protocol's corrections get skipped on
    /// exactly the branches where they were actually needed --
    /// breaking fidelity -- even though `execute`'s own exact,
    /// noise-free run of the identical `Program` still reaches ~100%.
    /// This is the test that actually proves `readout.rs`'s corruption
    /// model has a real physical effect once wired through `Gate::If`,
    /// not just that `corrupt_readout` computes the right probability
    /// in isolation (see `readout.rs`'s own unit tests for that half).
    #[test]
    fn readout_noise_can_break_a_correction_decision() {
        use crate::readout::ReadoutCalibration;
        use sirraya_qutub::DensityMatrix;

        // p01=0.0, p10=1.0: a true "0" is always reported correctly,
        // but a true "1" is *always* misreported as "0". Not a
        // realistic device number -- chosen specifically to make the
        // consequence deterministic (every trial where the true
        // outcome is 1 shows the effect, not just "most" trials).
        let broken_readout = ReadoutCalibration {
            name: "test: always misreports a true 1 as 0",
            backend: Backend::TrappedIon,
            p01: 0.0,
            p10: 1.0,
        };

        // Teleport |1> specifically: a deterministic target makes a
        // wrong final state easy to detect (fidelity clearly < 1.0),
        // unlike a state where "wrong" and "right" might coincide by
        // chance for some branches.
        let mut source = Circuit::new(3);
        source.push(Gate::X(0));
        source.push(Gate::H(1));
        source.push(Gate::Cx(1, 2));
        source.push(Gate::Cx(0, 1));
        source.push(Gate::H(0));
        source.num_clbits = 2;
        source.push(Gate::Measure(0, 0));
        source.push(Gate::Measure(1, 1));
        source.push(Gate::If(vec![(1, true)], Box::new(Gate::X(2))));
        source.push(Gate::If(vec![(0, true)], Box::new(Gate::Z(2))));

        let bc = backend::lower(&source, Backend::TrappedIon);
        let cal = trapped_ion_pulse_calibration();
        let program = compile(&bc, &cal).unwrap();

        let target: DensityMatrix = {
            let mut reg = QuantumRegister::new(1).unwrap();
            reg.apply_pauli_x(0).unwrap();
            reg.to_density_matrix().unwrap()
        };

        // Sanity check: the exact, noise-free path still reaches
        // ~100% -- confirms any failure below is readout.rs's doing,
        // not a pre-existing bug in compile/execute.
        let (reg_exact, _) = execute(&bc, &program, &cal).unwrap();
        let bob_exact = reg_exact.to_density_matrix().unwrap().partial_trace(&[2]).unwrap();
        assert!((bob_exact.fidelity(&target).unwrap() - 1.0).abs() < 1e-9);

        // With p01=0/p10=1, corrupt_readout's outcome doesn't depend
        // on the sample value at all (see readout.rs's own
        // certain_error_calibration_always_flips test), so any fixed
        // sample works here -- what varies trial to trial is the
        // *true* measurement outcome, which is still genuinely random.
        let mut saw_broken_fidelity = false;
        for _ in 0..30 {
            let (reg_noisy, _clbits) =
                execute_with_readout_noise(&bc, &program, &cal, &broken_readout, || 0.5).unwrap();
            let bob_noisy = reg_noisy.to_density_matrix().unwrap().partial_trace(&[2]).unwrap();
            let fidelity = bob_noisy.fidelity(&target).unwrap();
            if (fidelity - 1.0).abs() > 1e-6 {
                saw_broken_fidelity = true;
            }
        }
        // True (m0, m1) is uniform over 4 equally-likely branches; 3
        // of the 4 need at least one correction this calibration
        // always suppresses, so the chance all 30 trials land in the
        // one safe branch is (1/4)^30 -- not a realistic flake.
        assert!(
            saw_broken_fidelity,
            "expected at least one trial where corrupted readout broke teleportation fidelity, \
             but every trial still reached ~100% -- readout noise isn't reaching Gate::If's \
             branch decision"
        );
    }

    /// The gate-noise counterpart to
    /// `readout_noise_can_break_a_correction_decision`: proves
    /// `execute_with_noise`'s `gate_cal` parameter has a real,
    /// checkable effect on the *quantum* state, not just that
    /// `noise::sample_depolarizing_error` computes the right
    /// probability in isolation (see `noise.rs`'s own unit tests for
    /// that half).
    ///
    /// # Why this needs *real* randomness, not a fixed sample
    /// An earlier version of this test used a single fixed sample
    /// (`|| 0.5`) for every noise-eligible event, expecting a
    /// deterministic, guaranteed-probability Pauli kick to obviously
    /// break fidelity. It didn't -- fidelity came back indistinguishable
    /// from 1.0. That's not a bug in the noise model; it's a real,
    /// interesting property of teleportation itself: a *systematic*,
    /// always-identical Pauli inserted at every gate is exactly the
    /// kind of error the protocol's own feed-forward correction can
    /// absorb (teleportation is fundamentally a Pauli-frame-tracking
    /// protocol -- the same reason gate-teleportation-based
    /// fault-tolerant constructions work at all), and a Pauli applied
    /// twice to the same qubit cancels outright (`Y*Y = I`). Real
    /// depolarizing noise is independent-random *per event*, not a
    /// systematic insertion, so this test draws real per-event
    /// randomness (via the `rand` dev-dependency) to actually match
    /// what the noise model is supposed to represent, and checks
    /// *average* fidelity across many trials rather than a single
    /// deterministic run.
    #[test]
    fn gate_noise_degrades_average_teleportation_fidelity() {
        use crate::fidelity::PublishedCalibration;
        use rand::Rng;
        use sirraya_qutub::DensityMatrix;

        // p=1.0: every single noise-eligible event draws a real error
        // (uniformly X, Y, or Z -- never "no error"). Far more
        // aggressive than any real device's actual error rate,
        // deliberately, so the average degradation is clearly visible
        // without needing an enormous trial count.
        let always_error = PublishedCalibration {
            name: "test: guaranteed single- and two-qubit error",
            single_qubit_fidelity: 0.0,
            two_qubit_fidelity: 0.0,
        };

        let mut source = Circuit::new(3);
        source.push(Gate::H(0)); // |+> on q0
        source.push(Gate::H(1));
        source.push(Gate::Cx(1, 2));
        source.push(Gate::Cx(0, 1));
        source.push(Gate::H(0));
        source.num_clbits = 2;
        source.push(Gate::Measure(0, 0));
        source.push(Gate::Measure(1, 1));
        source.push(Gate::If(vec![(1, true)], Box::new(Gate::X(2))));
        source.push(Gate::If(vec![(0, true)], Box::new(Gate::Z(2))));

        let bc = backend::lower(&source, Backend::TrappedIon);
        let cal = trapped_ion_pulse_calibration();
        let program = compile(&bc, &cal).unwrap();

        let target: DensityMatrix = {
            let mut reg = QuantumRegister::new(1).unwrap();
            reg.apply_hadamard(0).unwrap();
            reg.to_density_matrix().unwrap()
        };

        // Sanity check: the exact path still reaches ~100% -- confirms
        // any degradation below is the noise model's doing, not a
        // pre-existing bug.
        let (reg_exact, _) = execute(&bc, &program, &cal).unwrap();
        let bob_exact = reg_exact.to_density_matrix().unwrap().partial_trace(&[2]).unwrap();
        assert!((bob_exact.fidelity(&target).unwrap() - 1.0).abs() < 1e-9);

        let mut rng = rand::thread_rng();
        let trials = 50;
        let mut total_fidelity = 0.0;
        for _ in 0..trials {
            let (reg_noisy, _clbits) =
                execute_with_noise(&bc, &program, &cal, Some(&always_error), None, || {
                    rng.gen::<f64>()
                })
                .unwrap();
            let bob_noisy = reg_noisy.to_density_matrix().unwrap().partial_trace(&[2]).unwrap();
            total_fidelity += bob_noisy.fidelity(&target).unwrap();
        }
        let avg_fidelity = total_fidelity / trials as f64;
        assert!(
            avg_fidelity < 0.9,
            "expected guaranteed-probability, genuinely random per-gate noise to substantially \
             degrade average teleportation fidelity across {} trials, got average {}",
            trials, avg_fidelity
        );
    }

    /// Confirms `execute_with_noise` really does combine both sources
    /// in one call -- readout noise alone (via `execute_with_readout_noise`)
    /// only ever breaks fidelity on *some* branches (see that test);
    /// adding guaranteed, genuinely-random gate noise on top (see
    /// `gate_noise_degrades_average_teleportation_fidelity`'s doc
    /// comment on why it must be genuinely random, not a fixed sample),
    /// via the general `execute_with_noise` entry point, must degrade
    /// average fidelity too, confirming the two parameters aren't
    /// silently exclusive of each other.
    #[test]
    fn execute_with_noise_applies_both_sources_together() {
        use crate::fidelity::PublishedCalibration;
        use crate::readout::ReadoutCalibration;
        use rand::Rng;
        use sirraya_qutub::DensityMatrix;

        let always_gate_error = PublishedCalibration {
            name: "test: guaranteed gate error",
            single_qubit_fidelity: 0.0,
            two_qubit_fidelity: 0.0,
        };
        let never_readout_error = ReadoutCalibration {
            name: "test: perfect readout",
            backend: Backend::TrappedIon,
            p01: 0.0,
            p10: 0.0,
        };

        let mut source = Circuit::new(3);
        source.push(Gate::H(1));
        source.push(Gate::Cx(1, 2));
        source.push(Gate::Cx(0, 1));
        source.push(Gate::H(0));
        source.num_clbits = 2;
        source.push(Gate::Measure(0, 0));
        source.push(Gate::Measure(1, 1));
        source.push(Gate::If(vec![(1, true)], Box::new(Gate::X(2))));
        source.push(Gate::If(vec![(0, true)], Box::new(Gate::Z(2))));

        let bc = backend::lower(&source, Backend::TrappedIon);
        let cal = trapped_ion_pulse_calibration();
        let program = compile(&bc, &cal).unwrap();

        let target: DensityMatrix = {
            let reg = QuantumRegister::new(1).unwrap();
            reg.to_density_matrix().unwrap()
        };

        let mut rng = rand::thread_rng();
        let trials = 50;
        let mut total_fidelity = 0.0;
        for _ in 0..trials {
            let (reg_noisy, _clbits) = execute_with_noise(
                &bc,
                &program,
                &cal,
                Some(&always_gate_error),
                Some(&never_readout_error),
                || rng.gen::<f64>(),
            )
            .unwrap();
            let bob_noisy = reg_noisy.to_density_matrix().unwrap().partial_trace(&[2]).unwrap();
            total_fidelity += bob_noisy.fidelity(&target).unwrap();
        }
        let avg_fidelity = total_fidelity / trials as f64;
        assert!(
            avg_fidelity < 0.9,
            "expected the gate-noise component of execute_with_noise to still degrade average \
             fidelity even with readout noise set to zero, got average {} across {} trials",
            avg_fidelity, trials
        );
    }

    /// The real payoff of multi-condition `Gate::If`: a genuine joint
    /// (AND) correction that single-bit `If`s can't express on their
    /// own -- `Circuit::validate` disallows nesting one `If` inside
    /// another, so "apply X iff m0==1 AND m1==1" has no way to be
    /// built from two separate single-condition `If`s at all. Checked
    /// two ways: symbolically, across all four outcome combinations on
    /// a circuit with *only* Measure+If (no other real gates, so the
    /// only source of a fired Drive pulse is the joint condition
    /// itself, and `expected_fired`'s conditioned-gates-only ground
    /// truth is a complete account -- see this test's own history: an
    /// earlier version added unconditional `X(0)`/`X(1)` prep gates
    /// into this same structural check and got a real, if unrelated,
    /// mismatch from those prep gates' own unconditional pulses, which
    /// `expected_fired` was never meant to account for), and against
    /// real quantum state (which correctly accounts for every gate,
    /// prep included) for two concrete, deterministically-prepared
    /// outcomes.
    #[test]
    fn multi_condition_if_expresses_a_genuine_joint_correction() {
        use sirraya_qutub::DensityMatrix;

        fn joint_correction_circuit(prep0: bool, prep1: bool) -> Circuit {
            let mut c = Circuit::new(3);
            if prep0 {
                c.push(Gate::X(0));
            }
            if prep1 {
                c.push(Gate::X(1));
            }
            c.num_clbits = 2;
            c.push(Gate::Measure(0, 0));
            c.push(Gate::Measure(1, 1));
            // No single-condition If, or pair of them, can express
            // this -- it's a genuine joint condition on one gate.
            c.push(Gate::If(vec![(0, true), (1, true)], Box::new(Gate::X(2))));
            c
        }

        let cal = trapped_ion_pulse_calibration();

        // Structural check: no prep gates, so the only thing that can
        // fire a real Drive pulse at all is the joint If itself --
        // q2 fires iff BOTH m0 and m1 are 1, never on "either," which
        // is exactly what a buggy AND-vs-OR mixup in compile_one's
        // jump-chaining would get wrong.
        let source = joint_correction_circuit(false, false);
        let bc = backend::lower(&source, Backend::TrappedIon);
        let program = compile(&bc, &cal).unwrap();
        for m0 in [0u8, 1] {
            for m1 in [0u8, 1] {
                let outcomes = [m0, m1];
                let mut fired = symbolic_run(&program, &outcomes);
                let mut expected = expected_fired(&bc, &outcomes);
                fired.sort();
                expected.sort();
                let should_fire = m0 == 1 && m1 == 1;
                assert_eq!(
                    !expected.is_empty(),
                    should_fire,
                    "m0={} m1={}: expected ground-truth firing to require BOTH bits set",
                    m0, m1
                );
                assert_eq!(fired, expected, "m0={} m1={}: Program disagreed with ground truth", m0, m1);
            }
        }

        // Physical check: deterministically prepare m0=1, m1=1 (the
        // one branch where the joint condition holds) and confirm q2
        // really did get flipped, via real execution -- not just the
        // symbolic branch trace above. Uses full density-matrix
        // fidelity, which correctly accounts for every gate in the
        // circuit (prep gates included), unlike the conditioned-gates-
        // only `expected_fired` used above.
        let target_flipped: DensityMatrix = {
            let mut reg = QuantumRegister::new(1).unwrap();
            reg.apply_pauli_x(0).unwrap();
            reg.to_density_matrix().unwrap()
        };
        let target_unflipped: DensityMatrix = {
            let reg = QuantumRegister::new(1).unwrap();
            reg.to_density_matrix().unwrap()
        };

        let source_both_true = joint_correction_circuit(true, true);
        let bc_both_true = backend::lower(&source_both_true, Backend::TrappedIon);
        let program_both_true = compile(&bc_both_true, &cal).unwrap();
        let (reg, clbits) = execute(&bc_both_true, &program_both_true, &cal).unwrap();
        assert_eq!(clbits, vec![1, 1]);
        let q2 = reg.to_density_matrix().unwrap().partial_trace(&[2]).unwrap();
        assert!((q2.fidelity(&target_flipped).unwrap() - 1.0).abs() < 1e-9);

        // And the negative case: m0=1, m1=0 -- condition doesn't hold,
        // q2 must stay unflipped.
        let source_one_true = joint_correction_circuit(true, false);
        let bc_one_true = backend::lower(&source_one_true, Backend::TrappedIon);
        let program_one_true = compile(&bc_one_true, &cal).unwrap();
        let (reg2, clbits2) = execute(&bc_one_true, &program_one_true, &cal).unwrap();
        assert_eq!(clbits2, vec![1, 0]);
        let q2_unflipped = reg2.to_density_matrix().unwrap().partial_trace(&[2]).unwrap();
        assert!((q2_unflipped.fidelity(&target_unflipped).unwrap() - 1.0).abs() < 1e-9);
    }
}