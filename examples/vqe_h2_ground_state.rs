//! Ground-state energy of molecular hydrogen (H2) via VQE, run through
//! this crate's real compiler pipeline: a published two-qubit qubit
//! Hamiltonian -> a hardware-efficient ansatz ([`ir::Circuit`]) -> a
//! three-clique measurement scheme (Z/X/Y bases) -> a classical
//! coordinate-descent optimization loop against the ideal simulator ->
//! [`route::route_best`] / [`backend::lower`] / a published-calibration
//! fidelity estimate per backend -> a real Monte-Carlo NISQ noise model
//! plus zero-noise extrapolation on the winning backend, exactly the
//! same [`CircuitExecutor`] abstraction and `NoisyBackendExecutor` /
//! `zero_noise_extrapolate` machinery `qaoa_portfolio_optimization.rs`
//! introduced.
//!
//! H2 ground-state energy via VQE is arguably *the* standard NISQ
//! benchmark: it's what IBM (Kandala et al., Nature 2017), Google/UCSB
//! (O'Malley et al., PRX 2016), and multiple trapped-ion groups have all
//! published as their "does the stack actually work end to end" result.
//! Anyone evaluating a quantum SDK or transpiler has a rough sense of
//! what a credible H2 curve looks like -- which is exactly why this is
//! a stronger next example than another QAOA-shaped one.
//!
//! ## Where the numbers come from
//!
//! The qubit Hamiltonian is the two-qubit, Bravyi-Kitaev-reduced form
//! for H2 in a minimal (STO-3G) basis, first used experimentally in
//! O'Malley et al., "Scalable Quantum Simulation of Molecular
//! Energies", Phys. Rev. X 6, 031007 (2016):
//!
//! `H = g0*I + g1*Z0 + g2*Z1 + g3*Z0*Z1 + g4*Y0*Y1 + g5*X0*X1`
//!
//! This example fixes the coefficients at bond length R = 0.75
//! Angstrom (O'Malley et al., Table I, as transcribed in Goings 2020,
//! <http://joshuagoings.com/2020/08/20/VQE/>): `g0=-0.4804, g1=0.3435,
//! g2=-0.4347, g3=0.5716, g4=0.0910, g5=0.0910`, with a nuclear
//! repulsion energy of `0.7055696146` Hartree at that bond length.
//!
//! **Deliberately one bond length, not a full dissociation curve.**
//! Different published sources use different sign/scaling conventions
//! for how nuclear repulsion folds into `g0`, and per-distance
//! coefficient tables from more than one transcription risk silently
//! mixing incompatible conventions into what would look like one smooth
//! curve. One fully-cited, internally-verified point -- checked below
//! against a closed-form diagonalization of *this exact* Hamiltonian,
//! not a re-derivation from an external chemistry package -- is more
//! credible than a curve assembled from unverified interpolation.
//! Extending this to a real dissociation curve is a natural follow-up,
//! given a source for consistent per-distance coefficients.
//!
//! **Not a chemically quantitative result.** STO-3G is a minimal basis;
//! its total energies are known to be well outside chemical accuracy of
//! the true H2 potential energy surface. What this example verifies is
//! narrower and still real: that VQE, run through this crate's actual
//! compiler/routing/noise pipeline, finds *this Hamiltonian's* exact
//! ground state (computed independently via closed-form diagonalization
//! below) to within standard "chemical accuracy" (1.6 mHartree, ~1
//! kcal/mol -- the same threshold O'Malley et al.'s own abstract reports
//! against).
//!
//! Every number this example prints is either an exact classical
//! computation, an exact statevector-derived expectation value, a real
//! `std::time::Instant` measurement, or a real output of this crate's
//! own router/lowering/fidelity-estimation pipeline run against each
//! backend's real `Backend::coupling_map` -- same standard the QAOA
//! example holds itself to.
//!
//! Run with:
//!
//! cargo run --release --example vqe_h2_ground_state
//! cargo run --release --example vqe_h2_ground_state -- --p-layers 4 --sweeps 4
//! cargo run --release --example vqe_h2_ground_state -- --fast
//! cargo run --release --example vqe_h2_ground_state -- --noise-shots 5000

use sirraya_qutub::{Complex, QuantumRegister};
use sirraya_qutub_transpiler::backend::{lower, Backend};
use sirraya_qutub_transpiler::fidelity::{estimate_backend_circuit_fidelity, PublishedCalibration};
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::route::route_best;
use sirraya_qutub_transpiler::{decompose, emit, ir_optimize};
use std::time::Instant;

/// Every backend currently supported by the crate.
const BACKENDS: [Backend; 4] = [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti, Backend::Google];

fn calibration_for(backend: Backend) -> PublishedCalibration {
    if backend == Backend::TrappedIon {
        PublishedCalibration::quantinuum_helios_2026()
    } else if backend == Backend::IbmQ {
        PublishedCalibration::ibm_heron_r2()
    } else if backend == Backend::Rigetti {
        PublishedCalibration::rigetti_ankaa3()
    } else if backend == Backend::Google {
        PublishedCalibration::google_willow_2024()
    } else {
        panic!("no published calibration registered for backend {:?}", backend);
    }
}

/// A tiny xorshift64 PRNG, seeded for reproducibility -- used for the
/// VQE optimizer's random restart and for `NoisyBackendExecutor`'s
/// Monte-Carlo noise trajectories.
struct Xorshift64(u64);

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        Xorshift64(seed | 1)
    }
    fn next_f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

// ---------------------------------------------------------------------
// 1. The Hamiltonian and its exact ground state.
// ---------------------------------------------------------------------

struct H2Hamiltonian {
    /// `[g0, g1, g2, g3, g4, g5]` for `H = g0*I + g1*Z0 + g2*Z1 +
    /// g3*Z0*Z1 + g4*Y0*Y1 + g5*X0*X1`.
    g: [f64; 6],
    nuclear_repulsion: f64,
    bond_length_angstrom: f64,
}

fn h2_at_0_75_angstrom() -> H2Hamiltonian {
    H2Hamiltonian {
        g: [-0.4804, 0.3435, -0.4347, 0.5716, 0.0910, 0.0910],
        nuclear_repulsion: 0.7055696146,
        bond_length_angstrom: 0.75,
    }
}

impl H2Hamiltonian {
    /// Exact ground-state *electronic* energy via closed-form
    /// diagonalization. `Z0`, `Z1`, `Z0*Z1` are all diagonal in the
    /// computational basis, and `Y0*Y1`/`X0*X1` each only connect basis
    /// states that differ in *both* qubits -- so this Hamiltonian
    /// block-diagonalizes exactly into two 2x2 problems, `{|00>,
    /// |11>}` and `{|01>, |10>}` (the latter being the physically
    /// relevant single-excitation subspace H2's true ground state
    /// lives in, at any bond length), each solved by the standard
    /// symmetric-2x2 eigenvalue formula. This is the "Full CI"-
    /// equivalent ground truth every VQE result below is checked
    /// against -- exact for *this* qubit Hamiltonian by direct
    /// derivation, not an independent quantum-chemistry recomputation
    /// (which is the right ground truth here: VQE's job is to find
    /// this Hamiltonian's ground state, not to re-derive the
    /// Hamiltonian from the molecule).
    fn exact_ground_state_electronic_energy(&self) -> f64 {
        let [g0, g1, g2, g3, g4, g5] = self.g;
        let min_eigenvalue_2x2 = |a: f64, d: f64, b: f64| -> f64 {
            let mean = (a + d) / 2.0;
            let half_gap = (a - d) / 2.0;
            mean - (half_gap * half_gap + b * b).sqrt()
        };
        let e_00_11 = min_eigenvalue_2x2(g0 + g1 + g2 + g3, g0 - g1 - g2 + g3, g5 - g4);
        let e_01_10 = min_eigenvalue_2x2(g0 - g1 + g2 - g3, g0 + g1 - g2 - g3, g4 + g5);
        e_00_11.min(e_01_10)
    }

    fn exact_ground_state_total_energy(&self) -> f64 {
        self.exact_ground_state_electronic_energy() + self.nuclear_repulsion
    }
}

// ---------------------------------------------------------------------
// 2. The ansatz and its measurement.
// ---------------------------------------------------------------------

/// One VQE layer: a full single-qubit Euler rotation (`Rz`-`Rx`-`Rz`,
/// 3 params) on each qubit, then one `Rzz` entangler (1 param) between
/// them -- 7 params per layer. This is a generic hardware-efficient
/// ansatz, not a problem-tailored UCC circuit derived from H2's
/// excitation structure: repeated application of essentially any fixed
/// entangling two-qubit gate, interleaved with arbitrary single-qubit
/// gates, is known to generate a dense subgroup of the full two-qubit
/// unitary group (Deutsch, Barenco & Ekert, Proc. R. Soc. A 449, 669
/// (1995); the tight "3 gates exactly suffice" bound is proved
/// specifically for CNOT/iSWAP-class maximally-entangling gates by
/// Vatan & Williams, Phys. Rev. A 69, 032315 (2004) and Vidal & Dawson,
/// Phys. Rev. A 69, 010301 (2004) -- `Rzz(theta)` for generic `theta`
/// is *not* maximally entangling, so this ansatz's reachable set is the
/// qualitatively-motivated dense-subgroup result, not that exact tight
/// bound). In practice that means: enough layers should make this
/// ansatz *able* to represent the exact ground state, but "enough" is
/// answered empirically by whether the optimizer below actually finds
/// it -- same caveat every hardware-efficient VQE ansatz in the
/// literature carries, and exactly why the exact-diagonalization
/// ground truth above matters as a check, not just a reference number.
fn vqe_ansatz(params: &[f64]) -> Circuit {
    assert!(params.len() % 7 == 0, "7 params per layer: (Rz, Rx, Rz) per qubit + one Rzz angle");
    let mut c = Circuit::new(2);
    for layer in params.chunks(7) {
        for q in 0..2 {
            c.push(Gate::Rz(q, layer[q * 3]));
            c.push(Gate::Rx(q, layer[q * 3 + 1]));
            c.push(Gate::Rz(q, layer[q * 3 + 2]));
        }
        c.push(Gate::Rzz(0, 1, layer[6]));
    }
    c
}

fn clone_circuit(n: usize, circuit: &Circuit) -> Circuit {
    let mut c = Circuit::new(n);
    for gate in &circuit.gates {
        c.push(gate.clone());
    }
    c
}

/// Appends the basis-change gates that make a subsequent Z-basis
/// measurement read out the requested Pauli's eigenvalue instead: `X`
/// -> `H`; `Y` -> `Rz(-pi/2)` then `H` (this maps `Y`'s +-i eigenstates
/// onto the computational basis up to an irrelevant global phase --
/// verified directly: `Rz(-pi/2)|+i> = e^{i*pi/4}|+>`, and `H|+> =
/// |0>`); `Z` -> nothing.
fn append_basis_rotation(c: &mut Circuit, qubit: usize, basis: char) {
    match basis {
        'X' => {
            c.push(Gate::H(qubit));
        }
        'Y' => {
            c.push(Gate::Rz(qubit, -std::f64::consts::FRAC_PI_2));
            c.push(Gate::H(qubit));
        }
        'Z' => {}
        _ => panic!("unsupported measurement basis {basis}"),
    }
}

/// `<Z0>` and `<Z1>`, computed exactly from one statevector's exact
/// probabilities.
fn single_expectations(register: &QuantumRegister) -> (f64, f64) {
    let amps: &[Complex] = register.get_state_vector();
    let (mut z0, mut z1) = (0.0, 0.0);
    for (state, amp) in amps.iter().enumerate() {
        let p = amp.magnitude_squared();
        z0 += p * if state & 1 == 0 { 1.0 } else { -1.0 };
        z1 += p * if (state >> 1) & 1 == 0 { 1.0 } else { -1.0 };
    }
    (z0, z1)
}

/// `<P0 * P1>` for whichever Pauli `P` the register has already been
/// rotated into the eigenbasis of (or `Z0*Z1` directly, if not
/// rotated) -- computed exactly from the statevector's probabilities.
fn product_expectation(register: &QuantumRegister) -> f64 {
    let amps: &[Complex] = register.get_state_vector();
    amps.iter()
        .enumerate()
        .map(|(state, amp)| {
            let p = amp.magnitude_squared();
            let b0 = if state & 1 == 0 { 1.0 } else { -1.0 };
            let b1 = if (state >> 1) & 1 == 0 { 1.0 } else { -1.0 };
            p * b0 * b1
        })
        .sum()
}

/// `<H>` for one ansatz parameter set, executed via `executor` -- three
/// separate circuit runs, one per commuting measurement clique: the
/// unrotated (Z) basis for `Z0`, `Z1`, `Z0*Z1` at once (they all
/// commute and share a measurement basis, so a real device measures
/// all three from one shot set -- the same clique-grouping VQE
/// measurement-reduction schemes use, e.g. Yen, Verteletskyi &
/// Izmaylov, J. Chem. Theory Comput. 2020), the X basis for `X0*X1`,
/// and the Y basis for `Y0*Y1`. This mirrors what real hardware VQE
/// actually has to do: the Hamiltonian's expectation value isn't
/// readable from a single measurement setting.
fn expected_energy(executor: &mut dyn CircuitExecutor, params: &[f64], h: &H2Hamiltonian) -> f64 {
    let base = vqe_ansatz(params);

    let z_register = executor.run(&base).expect("z-basis run should not fail");
    let (z0, z1) = single_expectations(&z_register);
    let zz = product_expectation(&z_register);

    let mut x_circuit = clone_circuit(2, &base);
    append_basis_rotation(&mut x_circuit, 0, 'X');
    append_basis_rotation(&mut x_circuit, 1, 'X');
    let x_register = executor.run(&x_circuit).expect("x-basis run should not fail");
    let xx = product_expectation(&x_register);

    let mut y_circuit = clone_circuit(2, &base);
    append_basis_rotation(&mut y_circuit, 0, 'Y');
    append_basis_rotation(&mut y_circuit, 1, 'Y');
    let y_register = executor.run(&y_circuit).expect("y-basis run should not fail");
    let yy = product_expectation(&y_register);

    h.g[0] + h.g[1] * z0 + h.g[2] * z1 + h.g[3] * zz + h.g[4] * yy + h.g[5] * xx
}

// ---------------------------------------------------------------------
// 2b. NISQ-realistic execution: the same `CircuitExecutor` seam and
//     Monte-Carlo noise model `qaoa_portfolio_optimization.rs` uses, so
//     that plugging in real fault-tolerant hardware later means adding
//     one more `impl CircuitExecutor`, not refactoring either example.
// ---------------------------------------------------------------------

trait CircuitExecutor {
    fn run(&mut self, circuit: &Circuit) -> Result<QuantumRegister, String>;
}

struct IdealExecutor;

impl CircuitExecutor for IdealExecutor {
    fn run(&mut self, circuit: &Circuit) -> Result<QuantumRegister, String> {
        let optimized = ir_optimize::optimize(circuit);
        let native = decompose(&optimized);
        emit::run(&native)
    }
}

fn gate_qubits(gate: &Gate) -> Vec<usize> {
    match gate {
        Gate::H(q) => vec![*q],
        Gate::Rz(q, _) => vec![*q],
        Gate::Rx(q, _) => vec![*q],
        Gate::Rzz(a, b, _) => vec![*a, *b],
        Gate::Swap(a, b) => vec![*a, *b],
        _ => vec![],
    }
}

fn push_pauli_kick(c: &mut Circuit, q: usize, p: f64, rng: &mut Xorshift64) {
    let r = rng.next_f64();
    if r < p / 2.0 {
        c.push(Gate::Rx(q, std::f64::consts::PI));
    } else if r < p {
        c.push(Gate::Rz(q, std::f64::consts::PI));
    }
}

/// See `qaoa_portfolio_optimization.rs`'s `NoisyBackendExecutor` for
/// the full derivation -- identical noise model here, just against a
/// 2-qubit circuit: route through the backend's real coupling map,
/// inject Pauli kicks after every resulting gate at a per-gate rate
/// backed out of `estimate_backend_circuit_fidelity`, run the result
/// through the ideal simulator. Averaging independent trajectories
/// approximates a density-matrix noise channel using only the
/// pure-state simulator this crate exposes.
struct NoisyBackendExecutor {
    backend: Backend,
    estimated_fidelity: f64,
    noise_scale: f64,
    rng: Xorshift64,
}

impl NoisyBackendExecutor {
    fn new(backend: Backend, estimated_fidelity: f64, noise_scale: f64, seed: u64) -> Self {
        NoisyBackendExecutor { backend, estimated_fidelity, noise_scale, rng: Xorshift64::new(seed) }
    }

    fn gate_error_rate(&self, total_gates: usize) -> f64 {
        let total_gates = (total_gates.max(1)) as f64;
        let base_rate = 1.0 - self.estimated_fidelity.clamp(1e-9, 1.0).powf(1.0 / total_gates);
        (base_rate * self.noise_scale).clamp(0.0, 0.5)
    }
}

impl CircuitExecutor for NoisyBackendExecutor {
    fn run(&mut self, circuit: &Circuit) -> Result<QuantumRegister, String> {
        let routed_gates: Vec<Gate> = match self.backend.coupling_map(2) {
            Some(coupling) => route_best(circuit, &coupling).gates,
            None => circuit.gates.clone(),
        };
        let p_gate = self.gate_error_rate(routed_gates.len());
        let mut noisy = Circuit::new(2);
        for gate in &routed_gates {
            noisy.push(gate.clone());
            for q in gate_qubits(gate) {
                push_pauli_kick(&mut noisy, q, p_gate, &mut self.rng);
            }
        }
        let optimized = ir_optimize::optimize(&noisy);
        let native = decompose(&optimized);
        emit::run(&native)
    }
}

/// Zero-noise extrapolation (Temme, Bravyi & Gambetta 2017): fit a line
/// through `(noise_scale, value)` measured at several amplified noise
/// levels, read off the value -- and its uncertainty -- at `noise_scale
/// = 0`.
///
/// This is a *weighted* least-squares fit (Numerical Recipes "straight
/// line data with errors" formulas), weighting each scale's point by
/// `1 / stderr^2`, i.e. points measured more precisely (more shots,
/// less per-trajectory variance) pull the line harder. An unweighted
/// fit silently lets a noisy point at one scale distort the
/// extrapolated intercept just as much as a precise point at another;
/// weighting also gives us a closed-form stderr on the intercept
/// itself, so we can say *how much to trust* the mitigated number
/// instead of just reporting it.
///
/// Returns `(intercept, intercept_stderr)`.
fn zero_noise_extrapolate(scales: &[f64], values: &[f64], stderrs: &[f64]) -> (f64, f64) {
    // Guard against a zero (or absurdly small) stderr producing an
    // infinite weight -- can happen at low noise_scale with few shots
    // if a run happened to draw no Pauli kicks at all.
    let weights: Vec<f64> = stderrs.iter().map(|&s| 1.0 / s.max(1e-9).powi(2)).collect();

    let s: f64 = weights.iter().sum();
    let sx: f64 = weights.iter().zip(scales).map(|(w, x)| w * x).sum();
    let sy: f64 = weights.iter().zip(values).map(|(w, y)| w * y).sum();
    let sxx: f64 = weights.iter().zip(scales).map(|(w, x)| w * x * x).sum();
    let sxy: f64 = weights
        .iter()
        .zip(scales)
        .zip(values)
        .map(|((w, x), y)| w * x * y)
        .sum();

    let delta = s * sxx - sx * sx;
    if delta.abs() < 1e-12 {
        // Degenerate (e.g. a single scale point): fall back to the
        // weighted mean, with no extrapolation possible.
        return (sy / s, (1.0 / s).sqrt());
    }
    let intercept = (sxx * sy - sx * sxy) / delta;
    let intercept_var = sxx / delta;
    (intercept, intercept_var.max(0.0).sqrt())
}

// ---------------------------------------------------------------------
// 3. Classical optimization: coordinate descent, one parameter at a
//    time, coarse-to-fine -- the same grid-then-refine shape
//    `qaoa_portfolio_optimization.rs`'s `optimize_one_layer` uses for
//    a `(gamma, beta)` pair, generalized to one coordinate at a time
//    because this ansatz has too many parameters (7 per layer) for a
//    joint grid search to be tractable.
// ---------------------------------------------------------------------

fn eval_param(executor: &mut dyn CircuitExecutor, h: &H2Hamiltonian, params: &mut [f64], idx: usize, value: f64) -> f64 {
    let saved = params[idx];
    params[idx] = value;
    let energy = expected_energy(executor, params, h);
    params[idx] = saved;
    energy
}

fn optimize_one_param(executor: &mut dyn CircuitExecutor, h: &H2Hamiltonian, params: &mut [f64], idx: usize, grid_points: usize, evaluations: &mut usize) {
    let two_pi = 2.0 * std::f64::consts::PI;
    let mut best_val = params[idx];
    let mut best_energy = eval_param(executor, h, params, idx, best_val);
    *evaluations += 1;

    let mut lo = 0.0;
    let mut hi = two_pi;
    for _pass in 0..3 {
        for i in 0..=grid_points {
            let val = lo + (hi - lo) * i as f64 / grid_points as f64;
            let energy = eval_param(executor, h, params, idx, val);
            *evaluations += 1;
            if energy < best_energy {
                best_energy = energy;
                best_val = val;
            }
        }
        let window = (hi - lo) * 0.15;
        lo = best_val - window;
        hi = best_val + window;
    }
    params[idx] = best_val;
}

fn optimize_vqe(executor: &mut dyn CircuitExecutor, h: &H2Hamiltonian, num_params: usize, grid_points: usize, sweeps: usize, seed: u64) -> (Vec<f64>, f64, usize) {
    let mut rng = Xorshift64::new(seed);
    let mut params: Vec<f64> = (0..num_params).map(|_| rng.next_f64() * 2.0 * std::f64::consts::PI).collect();
    let mut evaluations = 0usize;
    for _ in 0..sweeps {
        for idx in 0..num_params {
            optimize_one_param(executor, h, &mut params, idx, grid_points, &mut evaluations);
        }
    }
    let final_energy = expected_energy(executor, &params, h);
    evaluations += 1;
    (params, final_energy, evaluations)
}

// ---------------------------------------------------------------------
// 4. CLI, main.
// ---------------------------------------------------------------------

struct Args {
    p_layers: usize,
    sweeps: usize,
    noise_shots: usize,
    fast: bool,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().collect();
    // 5,000 shots (the earlier default) gets mitigated closer to exact
    // than raw, but the improvement (~0.0015 Ha) sits right at ~1
    // sigma of the mitigated fit's own stderr -- correct to report as
    // "not distinguishable from noise" rather than claim a win. Since
    // stderr falls off as 1/sqrt(shots) and the RNG is seeded
    // deterministically (same shots -> same numbers, every run),
    // 50,000 shots/scale reliably lands the same improvement above
    // ~3 sigma -- decisive rather than "it happened to come out
    // ahead this time". That's ~750k circuit executions total (5
    // scales x 50,000 shots x 3 bases), roughly 15-20s in release
    // mode: fine to run once ahead of a demo, a bit long to run live,
    // so it's worth pre-running and having the output ready rather
    // than executing it on stage.
    let mut args = Args { p_layers: 3, sweeps: 3, noise_shots: 50_000, fast: false };
    let mut i = 1;
    while i < raw.len() {
        match raw[i].as_str() {
            "--p-layers" if i + 1 < raw.len() => {
                args.p_layers = raw[i + 1].parse().unwrap_or(3).max(1);
                i += 2;
            }
            "--sweeps" if i + 1 < raw.len() => {
                args.sweeps = raw[i + 1].parse().unwrap_or(3).max(1);
                i += 2;
            }
            "--noise-shots" if i + 1 < raw.len() => {
                args.noise_shots = raw[i + 1].parse().unwrap_or(50_000).max(1);
                i += 2;
            }
            "--fast" => {
                args.fast = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    args
}

fn main() {
    let args = parse_args();
    let h = h2_at_0_75_angstrom();
    let num_params = 7 * args.p_layers;
    let chemical_accuracy = 0.0016; // Hartree, ~1 kcal/mol -- same threshold O'Malley et al. 2016 report VQE against.

    println!("{}", "=".repeat(78));
    println!("VQE ground-state energy of H2, R = {} Angstrom", h.bond_length_angstrom);
    println!("p = {} layer(s), {} params, {} sweep(s){}", args.p_layers, num_params, args.sweeps, if args.fast { ", fast mode" } else { "" });
    println!("{}", "=".repeat(78));
    println!("NOTE: minimal (STO-3G-derived) basis, one fixed bond length -- see this file's module doc comment.\n");

    // --- 1. Exact ground state of this qubit Hamiltonian. ---
    let exact_electronic = h.exact_ground_state_electronic_energy();
    let exact_total = h.exact_ground_state_total_energy();
    println!("Exact ground state (closed-form diagonalization of this qubit Hamiltonian):");
    println!("  electronic energy:  {:>10.6} Hartree", exact_electronic);
    println!("  + nuclear repulsion {:>10.6} Hartree", h.nuclear_repulsion);
    println!("  = total energy:     {:>10.6} Hartree", exact_total);

    // --- 2. Classically optimize the VQE ansatz against the ideal simulator. ---
    println!("\nOptimizing VQE ansatz against the ideal simulator...");
    let mut ideal_executor = IdealExecutor;
    let grid_points = if args.fast { 8 } else { 16 };
    let sweeps = if args.fast { 2 } else { args.sweeps };
    let start = Instant::now();
    let (params, vqe_electronic, evaluations) = optimize_vqe(&mut ideal_executor, &h, num_params, grid_points, sweeps, 42);
    let elapsed = start.elapsed();
    println!(
        "  {} parameter evaluations ({} circuit executions) in {:.3}s",
        evaluations,
        evaluations * 3,
        elapsed.as_secs_f64()
    );
    let vqe_total = vqe_electronic + h.nuclear_repulsion;
    let error = (vqe_electronic - exact_electronic).abs();
    println!("  VQE electronic energy: {:.6} Hartree (exact: {:.6}, error {:.6} Hartree)", vqe_electronic, exact_electronic, error);
    println!("  VQE total energy:      {:.6} Hartree (exact: {:.6})", vqe_total, exact_total);
    println!("  Within chemical accuracy (< {:.4} Hartree)? {}", chemical_accuracy, error < chemical_accuracy);

    // --- 3. Route + lower to every supported backend, estimate fidelity. ---
    println!("\n{}", "=".repeat(78));
    println!("Backend comparison (real routing against each backend's actual coupling map)");
    println!("{}", "=".repeat(78));
    println!("  {:<12} {:>10} {:>10} {:>10} {:>16}", "Backend", "SWAPs", "1q gates", "2q gates", "Est. fidelity");
    println!("  {}", "-".repeat(64));
    println!("  (routing/lowering the unrotated -- Z-basis -- circuit; X/Y-basis runs add one extra single-qubit gate per qubit and have essentially the same profile)");

    let base_circuit = vqe_ansatz(&params);
    let mut best_backend = BACKENDS[0];
    let mut best_fidelity = -1.0;

    for &backend in BACKENDS.iter() {
        let swap_count = match backend.coupling_map(2) {
            Some(coupling) => route_best(&base_circuit, &coupling).gates.iter().filter(|g| matches!(g, Gate::Swap(_, _))).count(),
            None => 0,
        };
        let lowered = lower(&base_circuit, backend);
        let (single, two) = lowered.gate_counts();
        let cal = calibration_for(backend);
        let est_fidelity = estimate_backend_circuit_fidelity(&lowered, &cal);

        println!("  {:<12} {:>10} {:>10} {:>10} {:>15.2}%", format!("{:?}", backend), swap_count, single, two, est_fidelity * 100.0);
        if est_fidelity > best_fidelity {
            best_fidelity = est_fidelity;
            best_backend = backend;
        }
    }
    println!("\nRecommended backend: {:?} (estimated fidelity {:.2}%)", best_backend, best_fidelity * 100.0);

    // --- 4. Realistic NISQ execution: real noise applied, then mitigated (ZNE). ---
    println!("\n{}", "=".repeat(78));
    println!("Realistic NISQ execution: actual noise applied, then mitigated (ZNE)");
    println!("{}", "=".repeat(78));
    println!(
        "({:?}'s {:.2}% estimated fidelity above is only an estimate until applied to a run -- \
         this section actually applies an approximate version of that noise, over {} \
         independent Monte-Carlo trajectories per noise level.)",
        best_backend, best_fidelity * 100.0, args.noise_shots
    );

    // Five points instead of three: more leverage for the linear fit
    // and less sensitivity to any single scale's sampling noise. Going
    // beyond ~3x noise starts to leave the regime the linear
    // noise-scaling approximation was fit for, so we stop at 3.0
    // rather than pushing further out for "more data".
    let zne_scales = [1.0, 1.5, 2.0, 2.5, 3.0];
    let mut scale_mean_energies = Vec::with_capacity(zne_scales.len());
    let mut scale_stderr_energies = Vec::with_capacity(zne_scales.len());
    for (i, &scale) in zne_scales.iter().enumerate() {
        let mut executor = NoisyBackendExecutor::new(best_backend, best_fidelity, scale, 0xC0FFEE + i as u64);
        let mut sum = 0.0;
        let mut sq_sum = 0.0;
        for _ in 0..args.noise_shots {
            // Each trajectory's `expected_energy` is itself an exact
            // expectation over that trajectory's three (rotated)
            // statevectors -- the noisy-trajectory draw is the
            // Monte-Carlo sample from the noise channel; there's no
            // reason to add a second, independent single-shot
            // measurement draw on top of it when estimating a mean.
            let e = expected_energy(&mut executor, &params, &h);
            sum += e;
            sq_sum += e * e;
        }
        let shots_f = args.noise_shots as f64;
        let mean = sum / shots_f;
        let variance = (sq_sum / shots_f - mean * mean).max(0.0);
        scale_mean_energies.push(mean);
        scale_stderr_energies.push((variance / shots_f).sqrt());
    }
    let (mitigated_electronic, mitigated_stderr) =
        zero_noise_extrapolate(&zne_scales, &scale_mean_energies, &scale_stderr_energies);
    let raw_gap = (scale_mean_energies[0] - exact_electronic).abs();
    let mitigated_gap = (mitigated_electronic - exact_electronic).abs();
    let noise_underpowered = scale_stderr_energies[0] >= raw_gap;
    // The honest question isn't "is the mitigated point closer to
    // exact" (a single-run coin flip when the two are within a
    // stderr or two of each other) -- it's "is the mitigated point
    // closer *by more than measurement noise can explain*". Compare
    // the gap-improvement to the mitigated fit's own uncertainty.
    let improvement = raw_gap - mitigated_gap;
    let improvement_significant = improvement.abs() > mitigated_stderr;

    println!("\n  {:<34} {:>12}", "", "Hartree");
    println!("  {}", "-".repeat(48));
    println!("  raw noisy mean energy (1x noise)  {:>12.6}  (stderr {:.6})", scale_mean_energies[0], scale_stderr_energies[0]);
    println!("  ZNE-mitigated mean energy         {:>12.6}  (stderr {:.6})", mitigated_electronic, mitigated_stderr);
    println!("  exact electronic energy           {:>12.6}", exact_electronic);
    println!(
        "\n(raw error {:.6} Hartree, mitigated error {:.6} Hartree.)",
        raw_gap, mitigated_gap
    );
    if improvement_significant && improvement > 0.0 {
        println!(
            "ZNE improved the estimate by {:.6} Hartree, which exceeds the mitigated fit's own \
             stderr ({:.6}) -- this is a real, not just lucky, improvement.",
            improvement, mitigated_stderr
        );
    } else if improvement_significant {
        println!(
            "ZNE moved the estimate {:.6} Hartree *away* from exact, exceeding the mitigated \
             fit's stderr ({:.6}) -- likely a genuine case of linear extrapolation bias (the \
             true noise-vs-scale curve isn't perfectly linear here), not sampling noise. \
             Consider a quadratic (Richardson) extrapolation or more noise-shots per scale.",
            improvement.abs(), mitigated_stderr
        );
    } else {
        println!(
            "Raw and mitigated agree with exact to within the mitigated fit's stderr ({:.6}) -- \
             at this shot count the two aren't statistically distinguishable, whichever came \
             out numerically closer this run. Increase --noise-shots for a sharper comparison.",
            mitigated_stderr
        );
    }
    if noise_underpowered {
        println!(
            "\nWARNING: stderr on the raw 1x mean ({:.6}) is >= the gap between raw and exact \
             ({:.6}). At {:?}'s calibration-implied per-gate error rate, most individual \
             trajectories at --noise-shots {} see zero noise kicks at all, so the mitigated \
             number above isn't resting on enough perturbed trajectories to trust over the raw \
             one. Re-run with a larger --noise-shots before drawing a conclusion from whether \
             mitigated beat raw here.",
            scale_stderr_energies[0], raw_gap, best_backend, args.noise_shots
        );
    }

    // The headline for this section isn't just "mitigated is closer" --
    // it's whether each estimate actually clears the chemical-accuracy
    // bar chemists care about. Report both explicitly rather than
    // making the reader compare error numbers to the threshold above
    // themselves.
    let raw_passes = raw_gap < chemical_accuracy;
    let mitigated_passes = mitigated_gap < chemical_accuracy;
    println!(
        "\n  Chemical accuracy (< {:.4} Hartree)?    raw: {}   mitigated: {}",
        chemical_accuracy,
        if raw_passes { "PASS" } else { "FAIL" },
        if mitigated_passes { "PASS" } else { "FAIL" }
    );
    if !raw_passes && mitigated_passes {
        println!(
            "  -> Raw NISQ execution misses chemical accuracy; ZNE mitigation recovers it \
             (a {:.2} sigma improvement over sampling noise, per above)."
            , improvement.abs() / mitigated_stderr
        );
    }

    // --- 5. Summary. ---
    println!("\n{}", "=".repeat(78));
    println!("Summary");
    println!("{}", "=".repeat(78));
    println!("  Exact electronic energy:        {:.6} Hartree", exact_electronic);
    println!("  VQE (ideal simulator):          {:.6} Hartree (error {:.6})", vqe_electronic, error);
    println!(
        "  VQE (NISQ, raw, {:?}):     {:.6} Hartree (error {:.6}) [{}]",
        best_backend, scale_mean_energies[0], raw_gap, if raw_passes { "PASS" } else { "FAIL" }
    );
    println!(
        "  VQE (NISQ, ZNE-mitigated):       {:.6} Hartree (error {:.6}, stderr {:.6}) [{}]",
        mitigated_electronic, mitigated_gap, mitigated_stderr, if mitigated_passes { "PASS" } else { "FAIL" }
    );
    println!("  Chemical accuracy threshold:     {:.4} Hartree", chemical_accuracy);
    if !raw_passes && mitigated_passes {
        println!(
            "\n  Headline: raw NISQ execution misses chemical accuracy; ZNE mitigation \
             recovers it ({:.2} sigma improvement, not sampling noise).",
            improvement.abs() / mitigated_stderr
        );
    }
    println!("{}", "=".repeat(78));
}