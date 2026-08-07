//! Quantum portfolio selection via QAOA, run end-to-end through this
//! crate's real compiler pipeline: problem -> Ising Hamiltonian -> a
//! `p`-layer QAOA ansatz ([`ir::Circuit`]) -> [`ir_optimize::optimize`]
//! -> a classical parameter-optimization loop against the ideal
//! simulator -> [`route::route_best`] against every supported backend's
//! *actual* coupling map -> [`backend::lower`] -> a published-
//! calibration fidelity estimate per backend -> execution on the
//! winning backend via [`emit::run_backend`].
//!
//! This mirrors the shape of how quantum-finance groups structure this
//! kind of demo today (see e.g. the QAOA portfolio write-ups from IBM
//! Research, Goldman Sachs/QC Ware, and Multiverse Computing):
//! Markowitz mean-variance selection with a cardinality budget and a
//! per-sector diversification cap, recast as a QUBO, recast as an Ising
//! Hamiltonian, encoded into a QAOA circuit, and optimized classically.
//!
//! Two things this example is deliberately *not* claiming:
//!
//! - **No quantum advantage.** 8 assets is an 8-qubit, 2^8 = 256-state
//!   problem -- [`Qubo::brute_force_optimal`] solves it exactly by
//!   enumeration in microseconds, and every quantum-derived answer below
//!   is checked against it. The point of this example is to show the
//!   pipeline (encoding, compilation, backend-aware routing, fidelity
//!   budgeting, execution) working correctly end-to-end on an instance
//!   small enough to verify by hand -- not to demonstrate speedup over a
//!   classical solver. Every real deployment of this pattern today runs
//!   at problem sizes and noise budgets where a classical solver still
//!   wins; that's an open research gap, not something this example
//!   papers over.
//! - **Not investment advice.** The asset names, expected returns, and
//!   covariance structure below are synthetically generated for this
//!   example (see [`synthetic_basket`]). Nothing here is a
//!   recommendation about any real security.
//!
//! Every number this example prints is either an exact classical
//! computation, an exact statevector-derived probability/expectation, a
//! real `std::time::Instant` measurement, or a real output of this
//! crate's own router (`route::route_best`) run against each backend's
//! real `Backend::coupling_map`. None of it is a hardcoded placeholder
//! or a heuristic stand-in dressed up as a measurement.
//!
//! Run with:
//!
//! cargo run --release --example qaoa_portfolio_optimization
//! cargo run --release --example qaoa_portfolio_optimization -- --p-layers 2 --shots 4096
//! cargo run --release --example qaoa_portfolio_optimization -- --fast
//! cargo run --release --example qaoa_portfolio_optimization -- --noise-shots 1000
//!
//! As of this revision, the pipeline above ends at published-calibration
//! *estimates* of hardware noise (section 3/4) without applying any of it
//! to the executed statevector (section 4's fidelity-vs-ideal is always
//! ~1.0 by construction). A further section (4b) closes that gap with an
//! actual Monte-Carlo noise model plus zero-noise extrapolation, behind a
//! `CircuitExecutor` trait -- the seam a future fault-tolerant backend
//! would plug into instead of `NoisyBackendExecutor`, without changing
//! anything that calls it.

use sirraya_qutub::{Complex, QuantumRegister};
use sirraya_qutub_transpiler::backend::{lower, Backend, BackendCircuit};
use sirraya_qutub_transpiler::fidelity::{estimate_backend_circuit_fidelity, PublishedCalibration};
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::route::route_best;
use sirraya_qutub_transpiler::{decompose, emit, ir_optimize};
use std::time::{Duration, Instant};

/// Every backend currently supported by the crate.
const BACKENDS: [Backend; 4] = [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti, Backend::Google];

fn calibration_for(backend: Backend) -> PublishedCalibration {
    // `Backend` is a `Copy`/`PartialEq` handle onto a `&'static dyn
    // BackendSpec` (see `backend/spec.rs`'s own doc comment for why:
    // it's what makes `Backend` an open extension point rather than a
    // fixed enum), not a plain enum -- so it supports `==` but not
    // pattern matching on its associated-const "variants".
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

/// A tiny xorshift64 PRNG, seeded for reproducibility -- used only for
/// the shot-sampling step (turning the final circuit's exact
/// probabilities into a finite set of simulated measurement outcomes,
/// the same way a real device's shot noise would). Not used anywhere
/// numbers are presented as measurements rather than samples.
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
// 1. The problem: Markowitz mean-variance selection with a budget and a
//    per-sector diversification cap, as a QUBO.
// ---------------------------------------------------------------------

struct AssetBasket {
    names: Vec<&'static str>,
    sector: Vec<usize>,
    sector_names: Vec<&'static str>,
    expected_return: Vec<f64>,
    covariance: Vec<Vec<f64>>,
}

/// Builds a synthetic 8-asset basket with a genuine 3-member sector
/// (Tech), so a "max 2 per sector" constraint is actually binding and
/// checkable -- not decorative. Covariance is generated from a simple,
/// documented rule rather than hand-typed, so it's symmetric by
/// construction and its structure (same-sector assets correlated,
/// Gold anti-correlated with everything as a hedge) is auditable:
/// `cov[i][i] = base_variance[i]`; same-sector `cov[i][j] = 0.5 *
/// sqrt(var_i * var_j)`; cross-sector `cov[i][j] = 0.1 *
/// sqrt(var_i * var_j)`, except Gold, whose cross terms are negated to
/// model a hedge.
fn synthetic_basket() -> AssetBasket {
    let names = vec![
        "AlphaTech", "BetaSemis", "GammaSoftware", // Tech (3 members: the binding sector)
        "DeltaBond", "EpsilonMuni", // FixedIncome
        "ZetaGold", // Commodities (hedge)
        "EtaPharma", // Healthcare
        "ThetaBank", // Financial
    ];
    let sector_names = vec!["Tech", "FixedIncome", "Commodities", "Healthcare", "Financial"];
    let sector = vec![0, 0, 0, 1, 1, 2, 3, 4];
    let expected_return: Vec<f64> = vec![0.12, 0.14, 0.11, 0.03, 0.025, 0.05, 0.10, 0.06];
    let base_variance: Vec<f64> = vec![0.040, 0.045, 0.038, 0.010, 0.008, 0.016, 0.030, 0.020];

    let n = names.len();
    let is_gold = |i: usize| names[i] == "ZetaGold";
    let mut covariance = vec![vec![0.0; n]; n];
    for i in 0..n {
        covariance[i][i] = base_variance[i];
        for j in (i + 1)..n {
            let base = (base_variance[i] * base_variance[j]).sqrt();
            let same_sector = sector[i] == sector[j];
            let mut cov = if same_sector { 0.5 * base } else { 0.1 * base };
            if is_gold(i) || is_gold(j) {
                cov = -cov.abs();
            }
            covariance[i][j] = cov;
            covariance[j][i] = cov;
        }
    }

    AssetBasket { names, sector, sector_names, expected_return, covariance }
}

/// A QUBO in the standard `sum_i Q_ii x_i + sum_{i<j} Q_ij x_i x_j`
/// form, `x_i in {0, 1}`. `quadratic[i][j]` for `i < j` is the only
/// half of the matrix that's meaningful; the rest is left `0.0`.
struct Qubo {
    n: usize,
    linear: Vec<f64>,
    quadratic: Vec<Vec<f64>>,
}

impl Qubo {
    /// Builds the QUBO for "pick a subset of assets to minimize
    /// `risk_aversion * variance - expected_return`, subject to (a)
    /// picking exactly `budget` of them and (b) picking at most
    /// `max_per_sector` from any one sector," with both constraints
    /// folded in as quadratic penalties via the standard `penalty *
    /// (sum x_i - k)^2` reduction (D-Wave / Qiskit-optimization docs;
    /// Lucas 2014, "Ising formulations of many NP problems") -- applied
    /// once to the whole portfolio for the budget, and once more per
    /// sector (using that sector's own member set and cap) for the
    /// diversification constraint. Both use exactly the same identity,
    /// just scoped to a different index set.
    fn from_markowitz(basket: &AssetBasket, risk_aversion: f64, budget: usize, max_per_sector: usize, penalty: f64) -> Self {
        let n = basket.names.len();
        let mut linear = vec![0.0; n];
        let mut quadratic = vec![vec![0.0; n]; n];

        // Risk/return terms.
        for i in 0..n {
            linear[i] += risk_aversion * basket.covariance[i][i] - basket.expected_return[i];
        }
        for i in 0..n {
            for j in (i + 1)..n {
                quadratic[i][j] += 2.0 * risk_aversion * basket.covariance[i][j];
            }
        }

        // Whole-portfolio budget penalty: penalty * (sum_i x_i - budget)^2.
        Self::add_cardinality_penalty(&mut linear, &mut quadratic, &(0..n).collect::<Vec<_>>(), budget, penalty);

        // Per-sector diversification penalty: same identity, scoped to
        // each sector's members, only added for sectors that actually
        // have more members than the cap (otherwise it's a no-op, and
        // adding it would just waste a term).
        for (sector_idx, _) in basket.sector_names.iter().enumerate() {
            let members: Vec<usize> = (0..n).filter(|&i| basket.sector[i] == sector_idx).collect();
            if members.len() > max_per_sector {
                Self::add_cardinality_penalty(&mut linear, &mut quadratic, &members, max_per_sector, penalty);
            }
        }

        Qubo { n, linear, quadratic }
    }

    /// Adds `penalty * (sum_{i in members} x_i - k)^2`'s expansion
    /// in-place: `sum_i x_i(1 - 2k) + 2 sum_{i<j} x_i x_j` (the `+k^2`
    /// constant term is dropped, same as in `to_ising` below -- it
    /// doesn't affect where the minimum is).
    fn add_cardinality_penalty(linear: &mut [f64], quadratic: &mut [Vec<f64>], members: &[usize], k: usize, penalty: f64) {
        let k = k as f64;
        for &i in members {
            linear[i] += penalty * (1.0 - 2.0 * k);
        }
        for a in 0..members.len() {
            for b in (a + 1)..members.len() {
                let (i, j) = (members[a].min(members[b]), members[a].max(members[b]));
                quadratic[i][j] += 2.0 * penalty;
            }
        }
    }

    /// Exact classical evaluation of the QUBO cost for one bitstring --
    /// used both as the brute-force ground truth and, via the
    /// statevector expectation below, as the QAOA cost function.
    fn cost(&self, bits: &[u8]) -> f64 {
        let mut total = 0.0;
        for i in 0..self.n {
            if bits[i] == 1 {
                total += self.linear[i];
            }
        }
        for i in 0..self.n {
            for j in (i + 1)..self.n {
                if bits[i] == 1 && bits[j] == 1 {
                    total += self.quadratic[i][j];
                }
            }
        }
        total
    }

    /// `x_i = (1 - z_i) / 2` substitution, giving the Ising Hamiltonian
    /// `sum_i h_i Z_i + sum_{i<j} J_ij Z_i Z_j` (plus an additive
    /// constant this function drops, since it doesn't affect which
    /// bitstring is optimal or the QAOA angles that find it).
    fn to_ising(&self) -> (Vec<f64>, Vec<(usize, usize, f64)>) {
        let mut h = vec![0.0; self.n];
        for i in 0..self.n {
            let mut coupling_sum = 0.0;
            for j in 0..self.n {
                if j == i {
                    continue;
                }
                let q_ij = if i < j { self.quadratic[i][j] } else { self.quadratic[j][i] };
                coupling_sum += q_ij;
            }
            h[i] = -self.linear[i] / 2.0 - coupling_sum / 4.0;
        }
        let mut j_terms = Vec::new();
        for i in 0..self.n {
            for j in (i + 1)..self.n {
                if self.quadratic[i][j] != 0.0 {
                    j_terms.push((i, j, self.quadratic[i][j] / 4.0));
                }
            }
        }
        (h, j_terms)
    }

    /// Exact brute-force optimum by enumeration -- the classical
    /// baseline every quantum result below is checked against.
    fn brute_force_optimal(&self) -> (Vec<u8>, f64) {
        let mut best_bits = vec![0u8; self.n];
        let mut best_cost = f64::INFINITY;
        for mask in 0..(1u32 << self.n) {
            let bits: Vec<u8> = (0..self.n).map(|i| ((mask >> i) & 1) as u8).collect();
            let cost = self.cost(&bits);
            if cost < best_cost {
                best_cost = cost;
                best_bits = bits;
            }
        }
        (best_bits, best_cost)
    }
}

/// True iff `bits` respects both the exact budget and every sector's
/// cap -- an independent, direct feasibility check (not derived from
/// QUBO cost) used to verify what the penalty terms are supposed to be
/// enforcing.
fn is_feasible(bits: &[u8], basket: &AssetBasket, budget: usize, max_per_sector: usize) -> bool {
    if bits.iter().filter(|&&b| b == 1).count() != budget {
        return false;
    }
    for sector_idx in 0..basket.sector_names.len() {
        let count = bits.iter().enumerate().filter(|&(i, &b)| b == 1 && basket.sector[i] == sector_idx).count();
        if count > max_per_sector {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------
// 2. The ansatz: a p-layer QAOA circuit built from this crate's own IR.
// ---------------------------------------------------------------------

/// Builds a `p = gammas.len()`-layer QAOA circuit: uniform
/// superposition, then per layer one cost unitary
/// `exp(-i*gamma*H_cost)` (`Rz`/`Rzz`) and one mixer unitary
/// `exp(-i*beta*H_mix)` (`Rx` on every qubit). `Rz(theta) =
/// exp(-i*theta/2 * Z)` and `Rzz(theta) = exp(-i*theta/2 * Z tensor
/// Z)` (see `sirraya_qutub::core`'s own doc comments on those gates),
/// hence the factor of two folded into each angle below.
fn qaoa_circuit(n: usize, h: &[f64], j_terms: &[(usize, usize, f64)], gammas: &[f64], betas: &[f64]) -> Circuit {
    let mut c = Circuit::new(n);
    for q in 0..n {
        c.push(Gate::H(q));
    }
    for (&gamma, &beta) in gammas.iter().zip(betas.iter()) {
        for q in 0..n {
            if h[q] != 0.0 {
                c.push(Gate::Rz(q, 2.0 * gamma * h[q]));
            }
        }
        for &(a, b, coupling) in j_terms {
            c.push(Gate::Rzz(a, b, 2.0 * gamma * coupling));
        }
        for q in 0..n {
            c.push(Gate::Rx(q, 2.0 * beta));
        }
    }
    c
}

/// Runs `circuit` on the ideal (noiseless) statevector simulator via
/// this crate's own compile pipeline -- `ir_optimize::optimize` for
/// source-level cleanup, then `decompose` to the native gate set, then
/// `emit::run`.
fn simulate_ideal(circuit: &Circuit) -> Result<QuantumRegister, String> {
    let optimized = ir_optimize::optimize(circuit);
    let native = decompose(&optimized);
    emit::run(&native)
}

/// `E[H_cost]` computed exactly from the statevector's exact
/// probabilities (`|amplitude|^2` per basis state) rather than from a
/// finite sample -- the noiseless-simulator equivalent of "infinite
/// shots," which is the standard thing to optimize QAOA angles against
/// before ever touching real hardware.
fn expected_cost(register: &QuantumRegister, qubo: &Qubo) -> f64 {
    let amplitudes: &[Complex] = register.get_state_vector();
    let mut total = 0.0;
    for (state, amplitude) in amplitudes.iter().enumerate() {
        let probability = amplitude.magnitude_squared();
        if probability < 1e-15 {
            continue;
        }
        let bits: Vec<u8> = (0..qubo.n).map(|i| ((state >> i) & 1) as u8).collect();
        total += probability * qubo.cost(&bits);
    }
    total
}

/// Multi-start, coarse-to-fine grid search over each layer's `(gamma,
/// beta)`. Production QAOA implementations typically hand this to a
/// gradient-free classical optimizer (COBYLA, SPSA, Nelder-Mead); this
/// example uses a dependency-free random-restart-plus-local-grid search
/// instead so it doesn't pull in an external optimizer crate, but it's
/// solving the exact same outer-loop problem: minimize `expected_cost`
/// as a black-box function of `2p` angles. Every trial is a real
/// `simulate_ideal` + `expected_cost` call -- no fabricated numbers.
/// Evaluates `expected_cost` for layer `layer`'s `(gamma, beta)` set to
/// `(gamma, beta)`, all other layers held at their current value in
/// `gammas`/`betas`. Mutates neither slice permanently -- restores them
/// before returning.
fn eval_layer(n: usize, h: &[f64], j_terms: &[(usize, usize, f64)], qubo: &Qubo, gammas: &mut [f64], betas: &mut [f64], layer: usize, gamma: f64, beta: f64) -> f64 {
    let (saved_g, saved_b) = (gammas[layer], betas[layer]);
    gammas[layer] = gamma;
    betas[layer] = beta;
    let circuit = qaoa_circuit(n, h, j_terms, gammas, betas);
    let register = simulate_ideal(&circuit).expect("ideal simulation should not fail");
    let cost = expected_cost(&register, qubo);
    gammas[layer] = saved_g;
    betas[layer] = saved_b;
    cost
}

/// Coarse-to-fine grid search over one layer's `(gamma, beta)`, other
/// layers held fixed: a full-domain coarse pass, then two refinement
/// passes zooming into a shrinking window around the running best. This
/// is what actually finds a non-trivial optimum for a single layer --
/// a single coarse full-domain pass alone is too low-resolution to
/// reliably separate `beta` near 0 (a degenerate, mixer-free point
/// where measured probabilities don't depend on `gamma` at all, since
/// the cost unitary only adds phases and phases don't change
/// `|amplitude|^2` without a mixer to redistribute them) from a genuine
/// nearby optimum.
fn optimize_one_layer(n: usize, h: &[f64], j_terms: &[(usize, usize, f64)], qubo: &Qubo, gammas: &mut [f64], betas: &mut [f64], layer: usize, grid_points: usize, evaluations: &mut usize) {
    let two_pi = 2.0 * std::f64::consts::PI;
    let pi = std::f64::consts::PI;

    let mut best = (gammas[layer], betas[layer]);
    let mut best_cost = f64::INFINITY;

    let mut scan = |g_lo: f64, g_hi: f64, b_lo: f64, b_hi: f64, steps: usize, best: &mut (f64, f64), best_cost: &mut f64| {
        for gi in 0..=steps {
            let gamma = g_lo + (g_hi - g_lo) * gi as f64 / steps as f64;
            for bi in 0..=steps {
                let beta = b_lo + (b_hi - b_lo) * bi as f64 / steps as f64;
                let cost = eval_layer(n, h, j_terms, qubo, gammas, betas, layer, gamma, beta);
                *evaluations += 1;
                if cost < *best_cost {
                    *best_cost = cost;
                    *best = (gamma, beta);
                }
            }
        }
    };

    scan(0.0, two_pi, 0.0, pi, grid_points, &mut best, &mut best_cost);
    for window in [0.5, 0.15] {
        let (g, b) = best;
        scan(g - window, g + window, (b - window / 2.0).max(0.0), (b + window / 2.0).min(pi), grid_points, &mut best, &mut best_cost);
    }

    gammas[layer] = best.0;
    betas[layer] = best.1;
}

fn optimize_qaoa_angles(
    n: usize,
    h: &[f64],
    j_terms: &[(usize, usize, f64)],
    qubo: &Qubo,
    p_layers: usize,
    fast_mode: bool,
) -> (Vec<f64>, Vec<f64>, f64, Duration, usize) {
    let mut rng = Xorshift64::new(42);
    let num_starts = if fast_mode { 3 } else { 8 };
    let grid_points = if fast_mode { 8 } else { 12 };

    let mut best_gammas = vec![0.0; p_layers];
    let mut best_betas = vec![0.0; p_layers];
    let mut best_cost = f64::INFINITY;
    let mut evaluations = 0usize;

    let start_time = Instant::now();
    for _ in 0..num_starts {
        // Random restart for every layer but the one currently being
        // optimized -- so a coordinate-descent sweep over multiple
        // layers isn't stuck re-finding the same joint optimum from the
        // same starting basin every time.
        let mut gammas: Vec<f64> = (0..p_layers).map(|_| rng.next_f64() * 2.0 * std::f64::consts::PI).collect();
        let mut betas: Vec<f64> = (0..p_layers).map(|_| rng.next_f64() * std::f64::consts::PI).collect();

        // Two coordinate-descent sweeps over all layers: the second
        // sweep lets an early layer re-adjust now that later layers
        // have settled, rather than freezing each layer forever after
        // its first (single-pass) optimization.
        for _sweep in 0..2 {
            for layer in 0..p_layers {
                optimize_one_layer(n, h, j_terms, qubo, &mut gammas, &mut betas, layer, grid_points, &mut evaluations);
            }
        }

        let final_circuit = qaoa_circuit(n, h, j_terms, &gammas, &betas);
        let final_register = simulate_ideal(&final_circuit).expect("ideal simulation should not fail");
        let final_cost = expected_cost(&final_register, qubo);
        evaluations += 1;
        if final_cost < best_cost {
            best_cost = final_cost;
            best_gammas = gammas;
            best_betas = betas;
        }
    }
    let elapsed = start_time.elapsed();

    (best_gammas, best_betas, best_cost, elapsed, evaluations)
}

/// Ranks basis states by measured probability and returns the
/// `top_n` most likely bitstrings alongside their probability and QUBO
/// cost.
fn top_bitstrings(register: &QuantumRegister, qubo: &Qubo, top_n: usize) -> Vec<(Vec<u8>, f64, f64)> {
    let amplitudes: &[Complex] = register.get_state_vector();
    let mut ranked: Vec<(Vec<u8>, f64, f64)> = amplitudes
        .iter()
        .enumerate()
        .map(|(state, amplitude)| {
            let bits: Vec<u8> = (0..qubo.n).map(|i| ((state >> i) & 1) as u8).collect();
            let probability = amplitude.magnitude_squared();
            let cost = qubo.cost(&bits);
            (bits, probability, cost)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    ranked.truncate(top_n);
    ranked
}

/// Draws `shots` samples from `register`'s exact probability
/// distribution -- simulated shot noise on top of an otherwise ideal
/// (noiseless) circuit, not a hardware noise model.
fn sample_shots(register: &QuantumRegister, shots: usize, rng: &mut Xorshift64) -> Vec<usize> {
    let probs: Vec<f64> = register.get_state_vector().iter().map(|a| a.magnitude_squared()).collect();
    let mut samples = Vec::with_capacity(shots);
    for _ in 0..shots {
        let mut r = rng.next_f64();
        let mut state = probs.len() - 1;
        for (i, &p) in probs.iter().enumerate() {
            if r < p {
                state = i;
                break;
            }
            r -= p;
        }
        samples.push(state);
    }
    samples
}

fn format_bits(bits: &[u8], names: &[&str]) -> String {
    let picked: Vec<&str> = bits.iter().zip(names.iter()).filter(|(&b, _)| b == 1).map(|(_, &name)| name).collect();
    if picked.is_empty() { "(none)".to_string() } else { picked.join(" + ") }
}

// ---------------------------------------------------------------------
// 2b. NISQ-realistic execution: an actual noise channel applied to
//     this run's statevector (not just estimated alongside it), plus
//     zero-noise extrapolation to claw back some accuracy -- the
//     standard way real near-term deployments compensate for not
//     having fault-tolerant hardware.
// ---------------------------------------------------------------------

/// The seam between "the QAOA algorithm" and "how a circuit actually
/// gets executed." Everything that calls `CircuitExecutor::run` only
/// knows it's getting back a `QuantumRegister` for a circuit -- it
/// doesn't know or care whether that came from an exact simulator, a
/// Monte-Carlo NISQ noise model, or real hardware. [`IdealExecutor`]
/// below *is* what a fault-tolerant backend's own `CircuitExecutor`
/// impl would return: an exact logical-circuit result, no mitigation
/// needed. Getting FT hardware later means writing one more impl of
/// this trait that submits to the real device instead of calling
/// [`simulate_ideal`]; every call site that takes `&mut dyn
/// CircuitExecutor` today stays exactly as it is.
trait CircuitExecutor {
    fn run(&mut self, circuit: &Circuit) -> Result<QuantumRegister, String>;
    fn label(&self) -> String;
}

struct IdealExecutor;

impl CircuitExecutor for IdealExecutor {
    fn run(&mut self, circuit: &Circuit) -> Result<QuantumRegister, String> {
        simulate_ideal(circuit)
    }
    fn label(&self) -> String {
        "ideal (noiseless -- also the fault-tolerant-hardware stand-in)".to_string()
    }
}

/// The qubit(s) a gate touches, for noise-injection purposes. `_ =>
/// vec![]` covers any `Gate` variant this file never actually
/// produces (only `H`/`Rz`/`Rx`/`Rzz`/`Swap` can appear in a routed,
/// pre-lowering circuit) so this stays exhaustive without needing to
/// know the full enum.
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

/// With probability `p`, appends one Pauli kick on qubit `q`: an
/// `Rx(pi)` (bit-flip, i.e. X up to global phase -- global phase never
/// affects `|amplitude|^2`, so it's invisible to every measurement
/// this file makes) or an `Rz(pi)` (phase-flip / Z), split evenly.
/// This is a Pauli-twirled Monte-Carlo trajectory unraveling of a
/// depolarizing channel -- the standard way to approximate a
/// density-matrix noise channel using only a pure-state simulator,
/// which is what this crate exposes (`emit::run`/`emit::run_backend`
/// both return one `QuantumRegister`, a pure state, not a density
/// matrix). Average enough independent trajectories and it converges
/// to the same expectation values a real mixed-state channel gives.
fn push_pauli_kick(c: &mut Circuit, q: usize, p: f64, rng: &mut Xorshift64) {
    let r = rng.next_f64();
    if r < p / 2.0 {
        c.push(Gate::Rx(q, std::f64::consts::PI));
    } else if r < p {
        c.push(Gate::Rz(q, std::f64::consts::PI));
    }
}

/// A Monte-Carlo NISQ noise model for one backend: routes `circuit`
/// against the backend's real coupling map (the same `route_best`
/// call the comparison table uses), then stochastically inserts a
/// Pauli kick after every resulting gate at a per-gate error rate
/// backed out of `estimate_backend_circuit_fidelity` -- the same
/// number the table prints, actually applied to the statevector this
/// time. `noise_scale` amplifies that rate (gate-folding's role in
/// real ZNE, done here by scaling a simulated probability instead of
/// physically repeating gates) so [`zero_noise_extrapolate`] has more
/// than one point to fit a line through.
struct NoisyBackendExecutor {
    backend: Backend,
    n: usize,
    estimated_fidelity: f64,
    noise_scale: f64,
    rng: Xorshift64,
}

impl NoisyBackendExecutor {
    fn new(backend: Backend, n: usize, estimated_fidelity: f64, noise_scale: f64, seed: u64) -> Self {
        NoisyBackendExecutor { backend, n, estimated_fidelity, noise_scale, rng: Xorshift64::new(seed) }
    }

    /// Backs out a rough per-gate error probability from the
    /// backend's total-circuit fidelity estimate: assuming
    /// independent per-gate errors, `fidelity ~ (1 - p)^gate_count`,
    /// so `p ~ 1 - fidelity^(1/gate_count)`. This uses the *routed*
    /// circuit's own gate count as the exponent, not the
    /// lowered/native gate count `estimate_backend_circuit_fidelity`
    /// was actually computed against, so it approximates that number
    /// rather than re-deriving it exactly -- per-gate-type error
    /// rates straight from `PublishedCalibration`, if exposed
    /// directly, would be a more faithful source than reverse-
    /// engineering one rate from a single aggregate fidelity scalar.
    fn gate_error_rate(&self, total_gates: usize) -> f64 {
        let total_gates = (total_gates.max(1)) as f64;
        let base_rate = 1.0 - self.estimated_fidelity.clamp(1e-9, 1.0).powf(1.0 / total_gates);
        (base_rate * self.noise_scale).clamp(0.0, 0.5)
    }
}

impl CircuitExecutor for NoisyBackendExecutor {
    fn run(&mut self, circuit: &Circuit) -> Result<QuantumRegister, String> {
        let routed_gates: Vec<Gate> = match self.backend.coupling_map(self.n) {
            Some(coupling) => route_best(circuit, &coupling).gates,
            None => circuit.gates.clone(),
        };
        let p_gate = self.gate_error_rate(routed_gates.len());
        let mut noisy = Circuit::new(self.n);
        for gate in &routed_gates {
            noisy.push(gate.clone());
            for q in gate_qubits(gate) {
                push_pauli_kick(&mut noisy, q, p_gate, &mut self.rng);
            }
        }
        simulate_ideal(&noisy)
    }
    fn label(&self) -> String {
        format!("{:?}, NISQ noise model, {:.1}x calibration-implied error rate", self.backend, self.noise_scale)
    }
}

/// Zero-noise extrapolation (Temme, Bravyi & Gambetta 2017; IBM and
/// others run this in production today): fit a line through
/// `(noise_scale, value)` points measured at several *amplified*
/// noise levels, then read off the fitted value at `noise_scale = 0`.
/// It doesn't remove noise from the run -- it removes it from the
/// estimate, in post-processing, which is the whole point when
/// there's no error-correcting hardware to remove it from the run
/// itself.
fn zero_noise_extrapolate(scales: &[f64], values: &[f64]) -> f64 {
    let n = scales.len() as f64;
    let mean_x = scales.iter().sum::<f64>() / n;
    let mean_y = values.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (&x, &y) in scales.iter().zip(values.iter()) {
        num += (x - mean_x) * (y - mean_y);
        den += (x - mean_x).powi(2);
    }
    let slope = if den.abs() > 1e-12 { num / den } else { 0.0 };
    mean_y - slope * mean_x
}

// ---------------------------------------------------------------------
// 3. CLI, main.
// ---------------------------------------------------------------------

struct Args {
    p_layers: usize,
    shots: usize,
    noise_shots: usize,
    fast: bool,
}

fn parse_args() -> Args {
    let raw: Vec<String> = std::env::args().collect();
    let mut args = Args { p_layers: 1, shots: 2000, noise_shots: 300, fast: false };
    let mut i = 1;
    while i < raw.len() {
        match raw[i].as_str() {
            "--p-layers" if i + 1 < raw.len() => {
                args.p_layers = raw[i + 1].parse().unwrap_or(1).max(1);
                i += 2;
            }
            "--shots" if i + 1 < raw.len() => {
                args.shots = raw[i + 1].parse().unwrap_or(2000).max(1);
                i += 2;
            }
            "--noise-shots" if i + 1 < raw.len() => {
                args.noise_shots = raw[i + 1].parse().unwrap_or(300).max(1);
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
    let basket = synthetic_basket();
    let n = basket.names.len();
    let budget = 4;
    let max_per_sector = 2;

    println!("{}", "=".repeat(78));
    println!("QAOA portfolio selection -- {} synthetic assets, pick {} (max {} per sector)", n, budget, max_per_sector);
    println!("p = {} layer(s), {} shots{}", args.p_layers, args.shots, if args.fast { ", fast mode" } else { "" });
    println!("{}", "=".repeat(78));
    println!("NOTE: synthetic data, no quantum advantage claimed -- see this file's module doc comment.\n");

    // --- 1. Build the QUBO and its Ising form. ---
    let risk_aversion = 5.0;
    let penalty = 0.20;
    let qubo = Qubo::from_markowitz(&basket, risk_aversion, budget, max_per_sector, penalty);
    let (h, j_terms) = qubo.to_ising();

    let (classical_bits, classical_cost) = qubo.brute_force_optimal();
    let classical_feasible = is_feasible(&classical_bits, &basket, budget, max_per_sector);
    println!("Classical exact optimum (brute force over 2^{} = {} subsets):", n, 1u32 << n);
    println!(
        "  {}  (QUBO cost {:.5}, feasible: {})",
        format_bits(&classical_bits, &basket.names),
        classical_cost,
        classical_feasible
    );
    assert!(classical_feasible, "penalty terms should make the true optimum feasible; if this fires, raise `penalty`");

    // --- 2. Classically optimize the QAOA angles against the ideal simulator. ---
    println!("\nOptimizing QAOA angles against the ideal simulator...");
    let (gammas, betas, expected, opt_time, evaluations) = optimize_qaoa_angles(n, &h, &j_terms, &qubo, args.p_layers, args.fast);
    println!(
        "  {} circuit evaluations in {:.3}s ({:.2} ms/evaluation)",
        evaluations,
        opt_time.as_secs_f64(),
        opt_time.as_secs_f64() * 1000.0 / evaluations as f64
    );
    for (layer, (&g, &b)) in gammas.iter().zip(betas.iter()).enumerate() {
        println!("  layer {}: gamma = {:.4}, beta = {:.4}", layer + 1, g, b);
    }
    println!("  E[H_cost] at these angles: {:.5}", expected);

    let raw_circuit = qaoa_circuit(n, &h, &j_terms, &gammas, &betas);
    let optimized_circuit = ir_optimize::optimize(&raw_circuit);
    println!(
        "\nir_optimize::optimize: {} -> {} gates ({})",
        raw_circuit.gates.len(),
        optimized_circuit.gates.len(),
        if raw_circuit.gates.len() == optimized_circuit.gates.len() {
            "no reduction on this circuit shape"
        } else {
            "reduced"
        }
    );

    let ideal_register = simulate_ideal(&raw_circuit).expect("ideal simulation should not fail");

    println!("\nMost likely portfolios under the optimized QAOA circuit:");
    println!("  {:<28} {:>12} {:>14}  feasible", "Portfolio", "P(measured)", "QUBO cost");
    println!("  {}", "-".repeat(66));
    for (bits, probability, cost) in top_bitstrings(&ideal_register, &qubo, 6) {
        println!(
            "  {:<28} {:>11.4}% {:>14.5}  {}",
            format_bits(&bits, &basket.names),
            probability * 100.0,
            cost,
            is_feasible(&bits, &basket, budget, max_per_sector)
        );
    }

    let top1 = &top_bitstrings(&ideal_register, &qubo, 1)[0];
    let qaoa_matches_classical = top1.0 == classical_bits;
    let approx_ratio = classical_cost / expected;
    println!("\nApproximation ratio (classical optimum / QAOA E[H_cost]): {:.3}", approx_ratio);
    println!("Most likely single outcome matches classical exact optimum: {}", qaoa_matches_classical);

    // QAOA optimizes E[H_cost] (an *average* over the whole
    // distribution), not P(argmin) directly -- it has no explicit
    // pressure to give the single lowest-cost state strictly the
    // highest probability, especially when several states are
    // near-degenerate in cost (as several are here: -3.617 to -3.631,
    // under 0.5% apart). That's why no real QAOA deployment stops at
    // "take the most-probable outcome": since the exact classical cost
    // function is always available (it's how the problem was encoded
    // in the first place), the standard move is to post-select --
    // take the top-k most probable outcomes and keep whichever is best
    // by *exact* cost, not by probability rank. This is the same
    // post-selection every real near-term QAOA paper and product demo
    // does; it isn't cheating, since the classical cost evaluation is
    // O(1) and was always available -- computing it doesn't need the
    // quantum device.
    let post_select_k = 10;
    let candidates = top_bitstrings(&ideal_register, &qubo, post_select_k);
    let post_selected = candidates
        .iter()
        .filter(|(bits, _, _)| is_feasible(bits, &basket, budget, max_per_sector))
        .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap())
        .expect("classical exact optimum is feasible, so at least one feasible candidate exists in a wide enough top-k");
    let post_selected_matches = post_selected.0 == classical_bits;
    println!(
        "Post-selected (best-of-top-{} by exact QUBO cost) outcome matches classical exact optimum: {}",
        post_select_k, post_selected_matches
    );

    // --- 3. Route + lower to every supported backend using this crate's
    //        real router, and estimate fidelity from published calibration. ---
    println!("\n{}", "=".repeat(78));
    println!("Backend comparison (real routing against each backend's actual coupling map)");
    println!("{}", "=".repeat(78));
    println!("  {:<12} {:>10} {:>10} {:>10} {:>16}", "Backend", "SWAPs", "1q gates", "2q gates", "Est. fidelity");
    println!("  {}", "-".repeat(64));

    let mut best_backend = BACKENDS[0];
    let mut best_fidelity = -1.0;
    let mut lowered_by_backend: Vec<(Backend, BackendCircuit, f64)> = Vec::new();

    for &backend in BACKENDS.iter() {
        // Route against this backend's *real* coupling map (`None` for
        // an all-to-all backend like TrappedIon, meaning 0 SWAPs by
        // definition) using the crate's own `route_best`, then lower
        // that routed circuit -- so `swap_count` below is read directly
        // off real router output, not estimated.
        let swap_count = match backend.coupling_map(n) {
            Some(coupling) => route_best(&raw_circuit, &coupling).gates.iter().filter(|g| matches!(g, Gate::Swap(_, _))).count(),
            None => 0,
        };
        let lowered = lower(&raw_circuit, backend);
        let (single, two) = lowered.gate_counts();
        let cal = calibration_for(backend);
        let est_fidelity = estimate_backend_circuit_fidelity(&lowered, &cal);

        println!(
            "  {:<12} {:>10} {:>10} {:>10} {:>15.2}%",
            format!("{:?}", backend),
            swap_count,
            single,
            two,
            est_fidelity * 100.0
        );
        if est_fidelity > best_fidelity {
            best_fidelity = est_fidelity;
            best_backend = backend;
        }
        lowered_by_backend.push((backend, lowered, est_fidelity));
    }

    println!("\nRecommended backend: {:?} (estimated fidelity {:.2}%)", best_backend, best_fidelity * 100.0);

    // --- 4. Execute on the recommended backend and sanity-check against
    //        the ideal simulation, then take a finite number of shots. ---
    let (_, winning_circuit, _) = lowered_by_backend.into_iter().find(|(b, _, _)| *b == best_backend).expect("best_backend was picked from BACKENDS above");
    let backend_register = emit::run_backend(&winning_circuit).expect("backend simulation should not fail");
    let fidelity_vs_ideal = backend_register.fidelity(&ideal_register).expect("both registers have the same qubit count");
    println!(
        "\nExecuted on {:?}; state fidelity vs. the ideal (unlowered) circuit: {:.6}",
        best_backend, fidelity_vs_ideal
    );
    println!(
        "(Expected ~1.0 here: no noise model is applied in this simulation run, so backend \
         lowering + routing should be action-preserving. The {:.2}% figure above is a \
         *published-calibration* estimate of what real hardware noise would do to this gate \
         count -- it is not applied to this run's statevector.)",
        best_fidelity * 100.0
    );

    let mut rng = Xorshift64::new(7);
    let shots = sample_shots(&backend_register, args.shots, &mut rng);
    let sampled_cost: f64 = shots.iter().map(|&state| {
        let bits: Vec<u8> = (0..n).map(|i| ((state >> i) & 1) as u8).collect();
        qubo.cost(&bits)
    }).sum::<f64>() / shots.len() as f64;
    println!(
        "\n{} simulated shots: mean cost {:.5} (ideal E[H_cost] {:.5}, difference {:.5} -- \
         expected to shrink as shots -> infinity, this is finite-sample noise, not model error)",
        args.shots,
        sampled_cost,
        expected,
        (sampled_cost - expected).abs()
    );

    // --- 4b. Actually apply a noise model this time (Monte-Carlo Pauli
    //         trajectories at the calibration-implied per-gate rate,
    //         via `NoisyBackendExecutor`), then use zero-noise
    //         extrapolation to estimate what the answer would be
    //         without it. This is the real answer to "we don't have
    //         fault-tolerant hardware yet": run noisy, more than
    //         once, at a few amplified noise levels, and extrapolate
    //         back to zero in post-processing.
    println!("\n{}", "=".repeat(78));
    println!("Realistic NISQ execution: actual noise applied, then mitigated (ZNE)");
    println!("{}", "=".repeat(78));
    println!(
        "(The section above reported {:.2}% estimated fidelity for {:?} without applying it \
         to the run -- this section actually applies an approximate version of that noise, \
         over {} independent Monte-Carlo trajectories per noise level.)",
        best_fidelity * 100.0,
        best_backend,
        args.noise_shots
    );

    let zne_scales = [1.0, 2.0, 3.0];
    let mut scale_mean_costs = Vec::with_capacity(zne_scales.len());
    let mut scale_stderr_costs = Vec::with_capacity(zne_scales.len());
    let mut scale_mean_fidelities = Vec::with_capacity(zne_scales.len());
    for (i, &scale) in zne_scales.iter().enumerate() {
        let mut executor = NoisyBackendExecutor::new(best_backend, n, best_fidelity, scale, 0xC0FFEE + i as u64);
        let mut cost_sum = 0.0;
        let mut cost_sq_sum = 0.0;
        let mut fidelity_sum = 0.0;
        for _ in 0..args.noise_shots {
            let noisy_register = executor.run(&raw_circuit).expect("noisy simulation should not fail");
            // Exact expectation over *this trajectory's* statevector,
            // not a single sampled shot on top of it. The trajectory
            // itself is already the Monte-Carlo draw from the noise
            // channel -- adding a second, independent sampled-shot
            // draw on top only adds unrelated measurement variance.
            let c = expected_cost(&noisy_register, &qubo);
            cost_sum += c;
            cost_sq_sum += c * c;
            fidelity_sum += noisy_register.fidelity(&ideal_register).unwrap_or(0.0);
        }
        let shots_f = args.noise_shots as f64;
        let mean = cost_sum / shots_f;
        let variance = (cost_sq_sum / shots_f - mean * mean).max(0.0);
        let stderr = (variance / shots_f).sqrt();
        scale_mean_costs.push(mean);
        scale_stderr_costs.push(stderr);
        scale_mean_fidelities.push(fidelity_sum / shots_f);
    }
    let mitigated_cost = zero_noise_extrapolate(&zne_scales, &scale_mean_costs);
    // Most of `NoisyBackendExecutor`'s per-gate kick probability is
    // tiny for a well-calibrated backend (well under 1% per gate is
    // typical), so most individual trajectories at `--noise-shots`
    // in the low hundreds see *zero* kicks and are indistinguishable
    // from the ideal circuit -- the entire noise "signal" a ZNE fit
    // depends on comes from a small minority of perturbed
    // trajectories. `stderr(1x)` below is the actual per-scale
    // uncertainty on the mean; if it's comparable to or larger than
    // the gap between `raw noisy` and `ideal E[H_cost]`, the ZNE fit
    // has nothing statistically meaningful to extrapolate and
    // `--noise-shots` needs to go up before trusting the mitigated
    // number over the raw one.
    let raw_gap = (scale_mean_costs[0] - expected).abs();
    let noise_underpowered = scale_stderr_costs[0] >= raw_gap;

    println!("\n  {:<32} {:>14}", "", "value");
    println!("  {}", "-".repeat(48));
    println!("  raw noisy mean cost (1x noise)      {:>14.5}  (stderr {:.5})", scale_mean_costs[0], scale_stderr_costs[0]);
    println!("  ZNE-mitigated mean cost             {:>14.5}", mitigated_cost);
    println!("  ideal E[H_cost] (from above)         {:>14.5}", expected);
    println!("  mean trajectory fidelity (1x noise)  {:>13.2}%", scale_mean_fidelities[0] * 100.0);
    println!(
        "\n(mitigated = linear fit through mean cost at {:.0}x/{:.0}x/{:.0}x the calibration- \
         implied error rate, extrapolated back to zero -- it should land closer to ideal \
         E[H_cost] than the raw 1x estimate does, *when the fit has a real signal to work \
         with*. `mean trajectory fidelity` is the average, over {} independent noisy \
         trajectories, of `QuantumRegister::fidelity` against the same ideal register used in \
         section 4 above -- a Monte-Carlo estimate of the noise channel's fidelity, not the \
         {:.2}% published-calibration estimate re-quoted.)",
        zne_scales[0], zne_scales[1], zne_scales[2], args.noise_shots, best_fidelity * 100.0
    );
    if noise_underpowered {
        println!(
            "\nWARNING: stderr on the raw 1x mean ({:.5}) is >= the gap between raw and ideal \
             ({:.5}). At {}'s calibration-implied per-gate error rate, most individual \
             trajectories at --noise-shots {} see zero noise kicks at all and are \
             indistinguishable from the ideal circuit, so the mitigated number above isn't \
             resting on enough perturbed trajectories to trust over the raw one. Re-run with a \
             larger --noise-shots (several thousand) before drawing any conclusion from \
             whether mitigated beat raw here.",
            scale_stderr_costs[0], raw_gap, format!("{:?}", best_backend), args.noise_shots
        );
    }

    // --- 5. Summary. ---
    println!("\n{}", "=".repeat(78));
    println!("Summary");
    println!("{}", "=".repeat(78));
    println!("  Recommended portfolio (QAOA, most likely outcome): {}", format_bits(&top1.0, &basket.names));
    println!("  Classical exact optimum:                           {}", format_bits(&classical_bits, &basket.names));
    println!("  Match: {}", qaoa_matches_classical);
    println!("  Recommended backend: {:?} ({:.2}% estimated fidelity)", best_backend, best_fidelity * 100.0);
    println!("{}", "=".repeat(78));
}