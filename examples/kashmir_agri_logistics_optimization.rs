//! Quantum agricultural-logistics assignment via QAOA, run end-to-end
//! through this crate's real compiler pipeline -- structurally the
//! *same* pipeline as `qaoa_portfolio_optimization.rs`: problem ->
//! Ising Hamiltonian -> a `p`-layer QAOA ansatz ([`ir::Circuit`]) ->
//! [`ir_optimize::optimize`] -> a classical parameter-optimization loop
//! against the ideal simulator -> [`route::route_best`] against every
//! supported backend's *actual* coupling map -> [`backend::lower`] -> a
//! published-calibration fidelity estimate per backend -> execution on
//! the winning backend via [`emit::run_backend`].
//!
//! # The problem
//!
//! This is the pilot the accompanying design note recommended: assign
//! each of several agricultural collection centers to one of several
//! controlled-atmosphere (CA) storage facilities, minimizing transport
//! cost plus a spoilage surcharge, subject to (a) every center choosing
//! exactly one option and (b) no facility exceeding its capacity. In
//! QUBO/QAOA terms this is a small facility-assignment problem: one
//! binary variable `x_{i,j}` per (center, facility) pair, laid out as
//! exactly `num_centers * num_facilities` qubits -- this crate's ideal
//! simulator caps circuits at 16 qubits, and this instance's 4x4
//! assignment already uses all of them, so there's no budget left for
//! extra (e.g. ancilla) qubits; see below for how the capacity
//! constraint works around that.
//!
//! It maps onto building blocks similar to the portfolio example's:
//!
//! - Transport + spoilage cost is linear in each `x_{i,j}` (no
//!   quadratic term is needed for the objective itself -- unlike the
//!   portfolio example's covariance term, there's no pairwise
//!   interaction between two different center-facility assignments in
//!   the cost function, only in the constraints below).
//! - The one-facility-per-center constraint is the *same* `penalty *
//!   (sum w_k x_k - target)^2` cardinality-penalty identity the
//!   portfolio example uses for its budget and per-sector caps
//!   (unweighted, `w = 1`, `target = 1`, since it's an equality: exactly
//!   one option per center). The facility-capacity constraint is a
//!   true `<=`, not an equality, so it uses a different identity --
//!   hard "forbidden combination" clauses, one per minimal
//!   capacity-violating combination of centers (see below and
//!   `add_forbidden_subset_penalty`).
//!
//! # Where the numbers come from
//!
//! This instance is grounded in real, cited Jammu & Kashmir horticulture
//! figures wherever a source was found, with every derived or assumed
//! number called out individually in [`kashmir_apple_instance`]'s own
//! doc comment. In outline:
//!
//! - **Center tonnages** are each district's *CA-storage-eligible*
//!   share of its real, reported annual apple production -- computed as
//!   `production x 0.30`, using the J&K Agriculture Production
//!   Department's own stated estimate (given in the UT Assembly, Nov
//!   2025) that CA-storage demand runs "about 30% of annual fruit
//!   production." Three of the four district production figures
//!   (Baramulla/Sopore, Shopian, Anantnag) are directly reported;
//!   Pulwama's is derived from a regional aggregate, flagged as such.
//! - **Facility capacities** are real reported CA-storage figures for
//!   Lassipora (Pulwama); the Aglar (Shopian) / Anantnag split is
//!   derived from a regional aggregate minus Lassipora, also flagged.
//! - **A fourth "no CA storage" option exists per center because the
//!   real numbers demand it.** Every source used here agrees J&K's
//!   actual CA capacity (~270,000-292,000 t as of 2025) falls well
//!   short of the government's own ~600,000 t estimated need, and
//!   growers who can't get a CA slot sell at a "distress" discount
//!   instead -- multiple sources describe this explicitly. Modeling
//!   every center as forced into a real facility would misrepresent the
//!   actual constraint; this pilot instead gives each center a fourth,
//!   effectively-uncapacitated "sell fresh at a discount" option, so
//!   the optimizer can legitimately leave some tonnage unstored rather
//!   than being forced into an infeasible instance. That widens each
//!   center's variable block from 3 facilities to 4 options, so this
//!   instance is 4 centers x 4 options = 16 qubits, not 12 -- exactly
//!   this crate's simulator's cap (see below).
//! - **Road distances, the transport/spoilage rate constants, and the
//!   distress-sale discount are still placeholders**, not sourced --
//!   see [`kashmir_apple_instance`] and `main` for exactly which numbers
//!   these are and why. They, plus a real Pulwama figure and a real
//!   Aglar/Anantnag capacity split, are what a real stakeholder dataset
//!   would replace.
//! - **The capacity constraint is a hard "forbidden combination" clause
//!   per facility, not a slack-qubit inequality and not (any longer) a
//!   soft equality penalty.** An earlier version of this file pulled
//!   each real facility's load *toward* its capacity with the same
//!   squared-penalty identity the one-hot constraint uses -- that's
//!   wrong for a `<=` constraint: `(load - capacity)^2` penalizes being
//!   *under* capacity just as much as being over it, and for this
//!   instance the true cost-optimal assignment leaves every real
//!   facility well under capacity (total demand across all four centers,
//!   ~369,437 t, exceeds total real CA capacity, ~257,000 t, so most
//!   tonnage is *supposed* to end up in "no CA storage" -- see below).
//!   That mismatch made the soft penalty fight the actual optimum no
//!   matter how it was scaled, which is why the classical brute-force
//!   check below used to fail its own feasibility assertion. The fix:
//!   for each real facility, enumerate every *minimal* combination of
//!   centers whose combined tonnage would exceed its capacity (a
//!   combination is minimal if no subset of it already exceeds
//!   capacity), and add a hard penalty forbidding that exact
//!   combination. Combinations of size 1-2 fold directly into the
//!   QUBO's linear/quadratic terms, so the Ising Hamiltonian and QAOA
//!   circuit see them exactly; this instance also has one size-3
//!   combination (Lassipora would overflow if Shopian + Pulwama +
//!   Anantnag were all routed there, even though no *pair* of them
//!   would), which can't fit a 2-local ansatz without an ancilla qubit
//!   -- and there's no ancilla budget left at 16/16 qubits (see
//!   above). Rather than drop that constraint, it's enforced exactly
//!   in `Qubo::cost()` alone via `higher_order_penalties`, which every
//!   *classical* evaluation of the QUBO (brute-force check, the QAOA
//!   optimizer's expected-cost scoring, post-selection) goes through --
//!   see that field's doc comment for the full reasoning and its
//!   tradeoff. The virtual "no storage" option is exempted from
//!   capacity clauses entirely, since it has no real capacity limit.
//! - **No quantum advantage claimed**, for the same reason as the
//!   portfolio example: 16 qubits is a 2^16 = 65,536-state problem,
//!   solved exactly by [`Qubo::brute_force_optimal`] in well under a
//!   second, and every
//!   quantum-derived answer below is checked against it.
//!
//! Every number this example prints is either a cited real figure, an
//! exact classical computation over those figures, an exact
//! statevector-derived probability/expectation, a real
//! `std::time::Instant` measurement, or a real output of this crate's
//! own router (`route::route_best`) run against each backend's real
//! `Backend::coupling_map`.
//!
//! Run with:
//!
//! cargo run --release --example kashmir_agri_logistics_optimization
//! cargo run --release --example kashmir_agri_logistics_optimization -- --p-layers 2 --shots 4096
//! cargo run --release --example kashmir_agri_logistics_optimization -- --fast
//! cargo run --release --example kashmir_agri_logistics_optimization -- --noise-shots 1000

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
    // BackendSpec`, not a plain enum -- see `qaoa_portfolio_optimization.rs`
    // for the full rationale -- so it's `==`-comparable but not
    // pattern-matchable.
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
/// the shot-sampling step and the noise-trajectory RNG, same role as in
/// the portfolio example.
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
// 1. The problem: collection-center-to-facility assignment under a
//    one-facility-per-center rule and per-facility capacity, as a QUBO.
// ---------------------------------------------------------------------

struct Center {
    name: &'static str,
    quantity_tonnes: f64,
}

struct Facility {
    name: &'static str,
    capacity_tonnes: f64,
    /// `None` for a real CA-storage facility, whose cost is computed
    /// from distance in [`assignment_cost`] and whose capacity is
    /// enforced by [`Qubo::from_assignment`]. `Some(discount_per_tonne)`
    /// for the virtual "no CA storage / distress fresh-market sale"
    /// option: skips the distance-based transport/spoilage cost
    /// entirely, charges a flat per-tonne value-loss cost instead, and
    /// is exempted from the capacity penalty (see module doc comment
    /// for why this option exists).
    distress_discount_per_tonne: Option<f64>,
}

struct LogisticsInstance {
    centers: Vec<Center>,
    facilities: Vec<Facility>,
    /// `distance_km[i][j]` = approximate road distance from center `i`
    /// to facility `j`. Unused (set to `0.0`) for any facility with
    /// `distress_discount_per_tonne = Some(_)`.
    distance_km: Vec<Vec<f64>>,
}

/// A 4-center / 4-option instance. See this file's module doc comment
/// for the overall sourcing policy; every number below is annotated
/// with exactly how it was obtained.
///
/// **Centers** -- `quantity_tonnes` is each district's CA-storage-
/// eligible apple tonnage (`annual production x 0.30`; see module doc
/// comment for the 30% figure's source). Annual production:
///
/// - Baramulla (through the Sopore mandi): 494,135 t -- reported
///   district-wise 2024-25 figures, Directorate of Horticulture via
///   Greater Kashmir, Jan 2025 ("Kashmir's Apple Supremacy").
/// - Shopian: 263,677 t -- same source.
/// - Anantnag: 261,964 t -- same source.
/// - Pulwama: ~211,680 t -- **not** directly reported in any source
///   found. Derived from Kashmir Life's Feb 2026 "South Kashmir apple
///   output nears 9.5 lakh MT" article, which gives Anantnag + Kulgam +
///   Pulwama + Shopian combined as 949,000 t; subtracting the Anantnag
///   and Shopian figures above leaves ~423,359 t for Pulwama + Kulgam,
///   split evenly here since Kulgam's own figure wasn't found either.
///   Flagged as an estimate, not a reported number.
///
/// **Facilities** -- CA-storage capacity:
///
/// - Lassipora CA Estate (Pulwama): 200,000 t -- Kashmir Life, Nov 2023
///   ("12 New Cold Storage Facilities Made Functional in Pulwama":
///   "increasing the total storage capacity at the estate to
///   approximately 2 lakh metric tons").
/// - Aglar CA Estate (Shopian) and the Anantnag CA store: not reported
///   individually. The same Feb 2026 Kashmir Life article gives South
///   Kashmir's *combined* CA capacity (Lassipora + Aglar + the one
///   Anantnag store it mentions) as 257,000 t; subtracting Lassipora's
///   200,000 t leaves 57,000 t, split here as 45,000 t (Aglar) / 12,000
///   t (Anantnag), reflecting Aglar being described as the larger of
///   the two remaining hubs. An estimate, not a reported split.
/// - "No CA storage" (virtual, capacity set arbitrarily large -- moot
///   anyway since [`Qubo::from_assignment`] exempts this facility from
///   the capacity penalty entirely): models the real, sourced fact that
///   J&K's CA capacity falls well short of demand and growers without a
///   slot sell at a discount instead. See module doc comment.
///
/// Road distances (km), the transport/spoilage rate constants in
/// `main`, and the distress-sale discount are placeholders, not
/// sourced -- order-of-magnitude figures based on general Kashmir-
/// valley geography and picked to be the same order of magnitude as
/// the transport/spoilage costs they compete against, same as the rest
/// of the numbers this file couldn't find a published figure for.
fn kashmir_apple_instance() -> LogisticsInstance {
    let centers = vec![
        Center { name: "Sopore (Baramulla)", quantity_tonnes: 148_240.5 }, // 494,135 t x 0.30
        Center { name: "Shopian", quantity_tonnes: 79_103.1 },             // 263,677 t x 0.30
        Center { name: "Pulwama", quantity_tonnes: 63_504.0 },             // ~211,680 t x 0.30 (derived)
        Center { name: "Anantnag", quantity_tonnes: 78_589.2 },            // 261,964 t x 0.30
    ];
    let facilities = vec![
        Facility { name: "Lassipora CA Estate (Pulwama)", capacity_tonnes: 200_000.0, distress_discount_per_tonne: None },
        Facility { name: "Aglar CA Estate (Shopian)", capacity_tonnes: 45_000.0, distress_discount_per_tonne: None },
        Facility { name: "Anantnag CA Store", capacity_tonnes: 12_000.0, distress_discount_per_tonne: None },
        Facility { name: "No CA storage (fresh/distress sale)", capacity_tonnes: 1.0e7, distress_discount_per_tonne: Some(400.0) },
    ];
    // distance_km[center][facility] -- placeholder approximate road
    // distances (not GPS-routed); the last column (virtual facility)
    // is unused.
    let distance_km = vec![
        vec![70.0, 78.0, 90.0, 0.0], // Sopore (Baramulla)
        vec![25.0, 5.0, 35.0, 0.0],  // Shopian
        vec![8.0, 20.0, 30.0, 0.0],  // Pulwama
        vec![30.0, 35.0, 5.0, 0.0],  // Anantnag
    ];
    LogisticsInstance { centers, facilities, distance_km }
}

/// Converts each center/facility pair into a per-assignment cost. For a
/// real CA facility: transport (`quantity * distance * cost_per_tonne_km`)
/// plus a spoilage surcharge (`quantity * transit_hours *
/// spoilage_per_tonne_hour`, with `transit_hours = distance /
/// avg_speed_kmph`). For the virtual "no CA storage" option: a flat
/// `quantity * discount_per_tonne` value-loss cost, no distance term.
/// Every term here is linear in `x_{i,j}`, which is why
/// [`Qubo::from_assignment`]'s cost contribution is a pure `linear[]`
/// term with no quadratic part.
fn assignment_cost(instance: &LogisticsInstance, cost_per_tonne_km: f64, spoilage_per_tonne_hour: f64, avg_speed_kmph: f64) -> Vec<Vec<f64>> {
    instance
        .centers
        .iter()
        .enumerate()
        .map(|(i, center)| {
            instance
                .facilities
                .iter()
                .enumerate()
                .map(|(j, facility)| {
                    if let Some(discount) = facility.distress_discount_per_tonne {
                        center.quantity_tonnes * discount
                    } else {
                        let d = instance.distance_km[i][j];
                        let transport = center.quantity_tonnes * d * cost_per_tonne_km;
                        let hours = d / avg_speed_kmph;
                        let spoilage = center.quantity_tonnes * hours * spoilage_per_tonne_hour;
                        transport + spoilage
                    }
                })
                .collect()
        })
        .collect()
}

/// A QUBO in the standard `sum_i Q_ii x_i + sum_{i<j} Q_ij x_i x_j`
/// form, `x_i in {0, 1}` -- identical shape to the portfolio example's
/// `Qubo`, just built from `from_assignment` instead of `from_markowitz`,
/// plus `higher_order_penalties` for constraints that don't fit that
/// 2-local shape (see its doc comment).
struct Qubo {
    n: usize,
    linear: Vec<f64>,
    quadratic: Vec<Vec<f64>>,
    /// Hard "not all of these bits can be 1 simultaneously" clauses of
    /// degree >= 3, evaluated exactly in `cost()` but *not* reflected
    /// in `linear`/`quadratic` or the Ising Hamiltonian `to_ising`
    /// builds from them. A degree-3+ clause needs an ancilla qubit
    /// chained through an AND-gate identity to become 2-local (see the
    /// module doc comment for the one this instance has), but this
    /// crate's ideal simulator caps circuits at 16 qubits and this
    /// instance's `num_centers * num_facilities` assignment qubits
    /// already use all 16 -- there's no room left for an ancilla on
    /// the physical circuit. So instead of growing `n`, any clause
    /// that can't fit `linear`/`quadratic` lands here, where it's
    /// still enforced exactly everywhere the *classical* cost is what
    /// matters (`brute_force_optimal`, the QAOA angle optimizer's
    /// `expected_cost`, and post-selection all call `cost()`). The
    /// QAOA ansatz itself just can't represent this specific
    /// constraint structurally -- its 2-local gates have no way to
    /// couple three qubits at once -- but that only limits how well it
    /// can *find* the true optimum, not whether the classical
    /// verification against it is correct.
    higher_order_penalties: Vec<(Vec<usize>, f64)>,
    /// The `+ penalty * target^2` term each cardinality-penalty call
    /// drops (see `add_weighted_cardinality_penalty`'s doc comment),
    /// summed back in here. It's a fixed number added to *every*
    /// bitstring's cost identically, so it changes no argmin anywhere
    /// in this file -- but without it, `cost()` reports a one-hot row
    /// with nothing assigned (`k = 0`) as contributing exactly the
    /// same 0 as a row with a *feasible* single assignment (`k = 1`),
    /// which is a real interpretability problem, not just a cosmetic
    /// one: printed QUBO costs for badly infeasible states (e.g. every
    /// center unassigned) then look deceptively competitive with -- or
    /// even cheaper than -- the true optimum, which is exactly the
    /// kind of number that should never reach a stakeholder. Restoring
    /// it makes every one-hot row contribute exactly
    /// `penalty * (k - 1)^2`: zero when satisfied, and a properly
    /// visible positive penalty otherwise -- so a fully feasible
    /// bitstring's `cost()` equals its real transport + spoilage cost
    /// exactly (every penalty term is zero), and every other
    /// bitstring's `cost()` is that real cost plus a visible, honest
    /// penalty for whatever it violates.
    constant: f64,
}

impl Qubo {
    /// Row-major qubit index for the (center, facility) pair.
    fn idx(num_facilities: usize, center: usize, facility: usize) -> usize {
        center * num_facilities + facility
    }

    /// Builds the assignment QUBO: minimize total transport + spoilage
    /// cost, subject to (a) exactly one facility per center and (b) no
    /// real facility's load exceeding its capacity. (b) used to be a
    /// soft `(load - capacity)^2` equality-style penalty; see the
    /// module doc comment for why that's wrong for a `<=` constraint
    /// and was replaced with hard "forbidden combination" clauses,
    /// most of which fold directly into `linear`/`quadratic` -- any
    /// that can't (degree 3+) go in `higher_order_penalties` instead;
    /// see that field's doc comment for why.
    fn from_assignment(instance: &LogisticsInstance, cost: &[Vec<f64>], one_hot_penalty: f64, capacity_clause_penalty: f64) -> Self {
        let nc = instance.centers.len();
        let nf = instance.facilities.len();
        let n = nc * nf;
        let mut linear = vec![0.0; n];
        let mut quadratic = vec![vec![0.0; n]; n];
        let mut higher_order_penalties: Vec<(Vec<usize>, f64)> = Vec::new();
        let mut constant = 0.0;

        // Transport + spoilage cost.
        for i in 0..nc {
            for j in 0..nf {
                linear[Self::idx(nf, i, j)] += cost[i][j];
            }
        }

        // Exactly one facility per center: unweighted cardinality-1
        // penalty over each center's own row of facility variables --
        // the same identity `add_cardinality_penalty` in the portfolio
        // example uses for its whole-portfolio budget, just scoped per
        // center instead of once globally.
        for i in 0..nc {
            let members: Vec<usize> = (0..nf).map(|j| Self::idx(nf, i, j)).collect();
            let weights = vec![1.0; nf];
            let target = 1.0;
            add_weighted_cardinality_penalty(&mut linear, &mut quadratic, &members, &weights, target, one_hot_penalty);
            constant += one_hot_penalty * target * target;
        }

        // Facility capacity: one hard "not all of these can be 1"
        // clause per minimal violating center-combination (see
        // `minimal_violating_subsets`). Degree 1-2 clauses fold
        // directly into `linear`/`quadratic`, so the Ising Hamiltonian
        // and QAOA circuit see them exactly; degree 3+ clauses (this
        // instance has exactly one, on Lassipora) go in
        // `higher_order_penalties` instead -- see that field's doc
        // comment for why. The virtual "no CA storage" facility has no
        // real capacity limit, so it never generates any clauses.
        for (j, facility) in instance.facilities.iter().enumerate() {
            if facility.distress_discount_per_tonne.is_some() {
                continue;
            }
            for subset in minimal_violating_subsets(instance, j) {
                let members: Vec<usize> = subset.iter().map(|&i| Self::idx(nf, i, j)).collect();
                match members.len() {
                    1 => linear[members[0]] += capacity_clause_penalty,
                    2 => {
                        let (a, b) = (members[0].min(members[1]), members[0].max(members[1]));
                        quadratic[a][b] += capacity_clause_penalty;
                    }
                    _ => higher_order_penalties.push((members, capacity_clause_penalty)),
                }
            }
        }

        Qubo { n, linear, quadratic, higher_order_penalties, constant }
    }

    /// Exact classical evaluation of the QUBO cost for one bitstring,
    /// including `higher_order_penalties` and `constant` (see their
    /// doc comments) -- so this is exact, and directly interpretable
    /// in real currency units when feasible, even for constraints the
    /// Ising/QAOA circuit can't structurally represent.
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
        for (members, penalty) in &self.higher_order_penalties {
            if members.iter().all(|&i| bits[i] == 1) {
                total += penalty;
            }
        }
        total + self.constant
    }

    /// `x_i = (1 - z_i) / 2` substitution, giving the Ising Hamiltonian
    /// `sum_i h_i Z_i + sum_{i<j} J_ij Z_i Z_j` (additive constant
    /// dropped, same as in the portfolio example).
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

/// Adds `penalty * (sum_{k in members} weights[k] * x_k - target)^2`'s
/// linear + quadratic expansion in place: linear term
/// `weights[k]^2 - 2*target*weights[k]` per member, quadratic term
/// `2*weights[a]*weights[b]` per unordered pair. The expansion's
/// `+ penalty * target^2` constant isn't a per-variable term at all
/// (it doesn't depend on `x`), so it can't go in `linear`/`quadratic`
/// -- callers that want the exact identity (not just its argmin, which
/// this omission doesn't change) add `penalty * target * target` to
/// their own constant themselves; see `Qubo::constant`'s doc comment
/// for why that matters here. All-`1.0` weights with `target = 1`
/// recovers the exact unweighted exactly-one-facility constraint.
fn add_weighted_cardinality_penalty(linear: &mut [f64], quadratic: &mut [Vec<f64>], members: &[usize], weights: &[f64], target: f64, penalty: f64) {
    for (idx, &i) in members.iter().enumerate() {
        let w = weights[idx];
        linear[i] += penalty * (w * w - 2.0 * target * w);
    }
    for a in 0..members.len() {
        for b in (a + 1)..members.len() {
            let (i, j) = (members[a].min(members[b]), members[a].max(members[b]));
            quadratic[i][j] += 2.0 * penalty * weights[a] * weights[b];
        }
    }
}

/// Every minimal center-combination that would push `facility`'s load
/// over its capacity: a combination is *minimal* if no proper subset
/// of it already exceeds capacity on its own. Enumerating only minimal
/// violations (out of all `2^centers` subsets) keeps the clause count
/// as small as possible -- any larger violating combination already
/// contains one of these as a sub-combination, so forbidding the
/// minimal ones forbids all of them. `centers.len()` is small (4 here),
/// so the brute-force `2^centers` subset scan is negligible.
fn minimal_violating_subsets(instance: &LogisticsInstance, facility: usize) -> Vec<Vec<usize>> {
    let nc = instance.centers.len();
    let capacity = instance.facilities[facility].capacity_tonnes;
    let load_of = |mask: u32| -> f64 { (0..nc).filter(|i| (mask >> i) & 1 == 1).map(|i| instance.centers[i].quantity_tonnes).sum() };

    let mut minimal = Vec::new();
    for mask in 1u32..(1u32 << nc) {
        if load_of(mask) <= capacity + 1e-9 {
            continue;
        }
        let is_minimal = (1u32..mask).filter(|&sub| sub & mask == sub).all(|sub| load_of(sub) <= capacity + 1e-9);
        if is_minimal {
            minimal.push((0..nc).filter(|i| (mask >> i) & 1 == 1).collect());
        }
    }
    minimal
}

/// True iff `bits` assigns every center to exactly one facility and no
/// facility's load exceeds its capacity -- an independent, direct
/// feasibility check (not derived from QUBO cost) used to verify what
/// the penalty terms are supposed to be enforcing.
fn is_feasible(bits: &[u8], instance: &LogisticsInstance) -> bool {
    let nc = instance.centers.len();
    let nf = instance.facilities.len();
    for i in 0..nc {
        let count = (0..nf).filter(|&j| bits[Qubo::idx(nf, i, j)] == 1).count();
        if count != 1 {
            return false;
        }
    }
    for j in 0..nf {
        let load: f64 = (0..nc).filter(|&i| bits[Qubo::idx(nf, i, j)] == 1).map(|i| instance.centers[i].quantity_tonnes).sum();
        if load > instance.facilities[j].capacity_tonnes + 1e-9 {
            return false;
        }
    }
    true
}

fn format_assignment(bits: &[u8], instance: &LogisticsInstance) -> String {
    let nf = instance.facilities.len();
    let parts: Vec<String> = instance
        .centers
        .iter()
        .enumerate()
        .map(|(i, center)| {
            let assigned: Vec<&str> = (0..nf).filter(|&j| bits[Qubo::idx(nf, i, j)] == 1).map(|j| instance.facilities[j].name).collect();
            let label = match assigned.len() {
                0 => "UNASSIGNED".to_string(),
                1 => assigned[0].to_string(),
                _ => format!("MULTIPLE({})", assigned.join(", ")),
            };
            format!("{} -> {}", center.name, label)
        })
        .collect();
    parts.join(" | ")
}

// ---------------------------------------------------------------------
// 2. The ansatz: a p-layer QAOA circuit built from this crate's own IR.
//    Identical in shape to the portfolio example -- the QAOA machinery
//    doesn't care what the underlying QUBO means.
// ---------------------------------------------------------------------

/// Rescales `h`/`j_terms` so that `optimize_one_layer`'s grid search
/// over `gamma in [0, 2*pi]` covers a meaningful range of the circuit's
/// diagonal unitary instead of aliasing through it. The one-hot and
/// capacity penalties needed to make the classical feasibility check
/// pass (see the module doc comment) put `to_ising`'s raw `h` values up
/// around 10^8-10^9; `Gate::Rz(2 * gamma * h[q])` at that scale wraps
/// around 2*pi many millions of times as gamma sweeps its intended
/// [0, 2*pi] range, so adjacent grid points in `optimize_one_layer`
/// land on essentially uncorrelated phases -- the angle search is
/// searching noise, not a landscape, which is why an earlier version
/// of this file found nothing better than a near-uniform mixture
/// (E[H_cost] in the billions, no feasible state anywhere near the
/// top of the distribution, versus a true optimum around 10^8).
/// Dividing every coefficient by the Hamiltonian's L1 norm (an upper
/// bound on its operator norm, chosen so gamma in [0, 2*pi] still
/// spans the circuit's full distinct behavior) fixes that without
/// changing which computational-basis state is optimal: this only
/// touches the angles fed into `Rz`/`Rzz` gates. Every place the
/// *actual* cost is computed or reported -- `expected_cost`,
/// `top_bitstrings`, `brute_force_optimal` -- goes through `qubo.cost()`
/// directly, using the original unscaled penalty terms, so reported
/// costs stay in real currency units regardless of this rescaling.
fn normalize_ising(h: &[f64], j_terms: &[(usize, usize, f64)]) -> (Vec<f64>, Vec<(usize, usize, f64)>) {
    let norm: f64 = h.iter().map(|x| x.abs()).sum::<f64>() + j_terms.iter().map(|&(_, _, c)| c.abs()).sum::<f64>();
    let norm = norm.max(1e-12);
    let h_scaled: Vec<f64> = h.iter().map(|x| x / norm).collect();
    let j_scaled: Vec<(usize, usize, f64)> = j_terms.iter().map(|&(a, b, c)| (a, b, c / norm)).collect();
    (h_scaled, j_scaled)
}

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

fn simulate_ideal(circuit: &Circuit) -> Result<QuantumRegister, String> {
    let optimized = ir_optimize::optimize(circuit);
    let native = decompose(&optimized);
    emit::run(&native)
}

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

    // `Rz(2*gamma*h)` is 2*pi-periodic in gamma, so a refinement window
    // straddling the 2*pi wraparound point (common when the coarse
    // scan's best sits near either edge of its [0, 2*pi] search range) can leave `best.0` outside it --
    // physically identical, but confusing to report: e.g. gamma =
    // 6.9332 is the same rotation as 0.6500, and printing the former
    // makes two layers that found the *same* angle look inexplicably
    // different. Wrapping back into the canonical range here is a pure
    // display normalization; it doesn't change `best_cost` or which
    // angle was actually found.
    gammas[layer] = best.0.rem_euclid(two_pi);
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
        let mut gammas: Vec<f64> = (0..p_layers).map(|_| rng.next_f64() * 2.0 * std::f64::consts::PI).collect();
        let mut betas: Vec<f64> = (0..p_layers).map(|_| rng.next_f64() * std::f64::consts::PI).collect();

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

// ---------------------------------------------------------------------
// 2b. NISQ-realistic execution: an actual noise channel applied to this
//     run's statevector, plus zero-noise extrapolation. Identical to
//     the portfolio example -- this section is entirely problem-
//     agnostic, since it only ever sees a `Circuit`.
// ---------------------------------------------------------------------

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
    let instance = kashmir_apple_instance();
    let nc = instance.centers.len();
    let nf = instance.facilities.len();
    let n = nc * nf;

    println!("{}", "=".repeat(78));
    println!("QAOA agricultural logistics pilot -- {} collection centers, {} options each ({} qubits)", nc, nf, n);
    println!("Kashmir apple CA-storage allocation -- district tonnages & CA capacities are real, cited figures");
    println!("p = {} layer(s), {} shots{}", args.p_layers, args.shots, if args.fast { ", fast mode" } else { "" });
    println!("{}", "=".repeat(78));
    println!(
        "NOTE: center/facility names, CA capacities, and the 30% CA-eligibility\n\
         figure are real, cited J&K horticulture-department numbers (Pulwama's\n\
         tonnage and the Aglar/Anantnag capacity split are derived estimates, not\n\
         directly reported). Road distances, transport/spoilage rates, and the\n\
         distress-sale discount are still illustrative placeholders. See\n\
         kashmir_apple_instance()'s doc comment for exactly which is which.\n\
         Cost figures below are in illustrative currency units, not sourced prices.\n\
         One facility-capacity constraint here can't be represented on the QAOA\n\
         circuit itself (would need a 17th qubit; this crate's simulator caps out\n\
         at 16) -- see the module doc comment and Qubo::higher_order_penalties.\n"
    );

    // --- 1. Build the QUBO and its Ising form. ---
    let cost_per_tonne_km = 3.5; // placeholder, currency/tonne-km
    let spoilage_per_tonne_hour = 15.0; // placeholder, currency/tonne-hour
    let avg_speed_kmph = 30.0;
    let cost = assignment_cost(&instance, cost_per_tonne_km, spoilage_per_tonne_hour, avg_speed_kmph);

    // `one_hot_penalty` just needs to dominate the cost swing between
    // options for one center (tens of millions here) -- its effective
    // scale is `penalty` itself, independent of tonnage, since its
    // members are unweighted (`w = 1`). `capacity_clause_penalty` is a
    // flat per-clause penalty (not tonnage-weighted, unlike the old
    // equality penalty this replaced), so it just needs to dominate
    // that same cost scale too; both are set well above the ~10^7-10^8
    // cost range this instance produces.
    let one_hot_penalty = 2.0e8;
    let capacity_clause_penalty = 1.0e9;
    let qubo = Qubo::from_assignment(&instance, &cost, one_hot_penalty, capacity_clause_penalty);
    let (h, j_terms) = qubo.to_ising();
    // See `normalize_ising`'s doc comment: only the angle-optimization /
    // circuit-building path uses the scaled Hamiltonian below. `qubo`
    // (unscaled) is what `expected_cost`/`top_bitstrings`/
    // `brute_force_optimal` use for actual reported costs throughout.
    let (h_scaled, j_scaled) = normalize_ising(&h, &j_terms);

    let (classical_bits, classical_cost) = qubo.brute_force_optimal();
    let classical_feasible = is_feasible(&classical_bits, &instance);
    println!("Classical exact optimum (brute force over 2^{} = {} assignments):", n, 1u64 << n);
    println!(
        "  {}  (total cost {:.2}, feasible: {})",
        format_assignment(&classical_bits, &instance),
        classical_cost,
        classical_feasible
    );
    println!(
        "  This cost is the real transport + spoilage total in illustrative currency units, not a \
         QUBO-internal number: every constraint-penalty term is exactly zero on a feasible \
         assignment (see Qubo::constant), so a feasible bitstring's QUBO cost and its real cost \
         coincide exactly."
    );
    assert!(
        classical_feasible,
        "penalty terms should make the true optimum feasible; if this fires, raise one_hot_penalty \
         and/or capacity_clause_penalty (see the scaling comment above)"
    );

    // How narrow a target this instance's constraints actually are --
    // used below to explain (rather than just assert away) shallow
    // QAOA's odds of landing on one by chance.
    let feasible_state_count = (0..(1u32 << n)).filter(|&mask| is_feasible(&(0..n).map(|i| ((mask >> i) & 1) as u8).collect::<Vec<u8>>(), &instance)).count();

    // --- 2. Classically optimize the QAOA angles against the ideal simulator. ---
    println!("\nOptimizing QAOA angles against the ideal simulator...");
    let (gammas, betas, expected, opt_time, evaluations) = optimize_qaoa_angles(n, &h_scaled, &j_scaled, &qubo, args.p_layers, args.fast);
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

    let raw_circuit = qaoa_circuit(n, &h_scaled, &j_scaled, &gammas, &betas);
    let optimized_circuit = ir_optimize::optimize(&raw_circuit);
    println!(
        "\nir_optimize::optimize: {} -> {} gates ({})",
        raw_circuit.gates.len(),
        optimized_circuit.gates.len(),
        if raw_circuit.gates.len() == optimized_circuit.gates.len() { "no reduction on this circuit shape" } else { "reduced" }
    );

    let ideal_register = simulate_ideal(&raw_circuit).expect("ideal simulation should not fail");

    println!("\nMost likely assignments under the optimized QAOA circuit:");
    for (bits, probability, cost) in top_bitstrings(&ideal_register, &qubo, 6) {
        println!(
            "  {:>9.4}%  cost {:>10.2}  feasible {:<5}  {}",
            probability * 100.0,
            cost,
            is_feasible(&bits, &instance),
            format_assignment(&bits, &instance)
        );
    }

    let top1 = &top_bitstrings(&ideal_register, &qubo, 1)[0];
    let qaoa_matches_classical = top1.0 == classical_bits;
    let approx_ratio = classical_cost / expected;
    println!(
        "\nApproximation ratio (classical optimum cost / QAOA E[H_cost]): {:.3} (1.0 = QAOA's average \
         outcome costs the same as the true optimum; since every bitstring's cost() is now anchored \
         at the real transport+spoilage total when feasible and only adds visible penalty otherwise \
         [see Qubo::constant], this is a standard 0-1 ratio, not raw QUBO-internal units)",
        approx_ratio
    );
    println!("Most likely single outcome matches classical exact optimum: {}", qaoa_matches_classical);

    // Post-selection: same reasoning as the portfolio example -- QAOA
    // optimizes E[H_cost], not P(argmin) directly, so take the top-k
    // most probable outcomes and keep whichever is feasible and best by
    // *exact* cost, not by probability rank. With only p=1 layer and a
    // small fraction of this instance's 65,536 states satisfying every
    // constraint (see `feasible_state_count` above), there's no
    // guarantee a shallow circuit concentrates enough probability on
    // any of them to show up in a top-k of any practical size --
    // that's a genuine, expected limitation of shallow QAOA on a
    // tightly-constrained instance (see the module doc comment's "no
    // quantum advantage claimed" note), not a bug, so this doesn't
    // panic if it happens: it reports the miss honestly, the same way
    // `qaoa_matches_classical` above reports a plain `false` rather
    // than asserting `true`.
    let post_select_k = 64;
    let candidates = top_bitstrings(&ideal_register, &qubo, post_select_k);
    let post_selected = candidates.iter().filter(|(bits, _, _)| is_feasible(bits, &instance)).min_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
    match post_selected {
        Some(selected) => {
            let overhead_pct = (selected.2 - classical_cost) / classical_cost * 100.0;
            println!(
                "Post-selected (best-of-top-{} by exact QUBO cost) outcome matches classical exact optimum: {}",
                post_select_k,
                selected.0 == classical_bits
            );
            println!(
                "  Post-selected assignment: {} (cost {:.2}, {:.1}% above the classical optimum)",
                format_assignment(&selected.0, &instance),
                selected.2,
                overhead_pct
            );
        }
        None => {
            println!(
                "Post-selected (best-of-top-{} by exact QUBO cost): no feasible candidate found. \
                 Only {} of this instance's {} states satisfy every constraint, and at p={} the \
                 QAOA circuit didn't put enough probability on any of them to surface in the top \
                 {} most likely outcomes -- a real limitation of an ansatz this shallow on a \
                 tightly-constrained instance, not a bug. Falling back to the classical exact \
                 optimum below.",
                post_select_k, feasible_state_count, 1u64 << n, args.p_layers, post_select_k
            );
        }
    }
    // `post_selected_matches` and `post_selected_bits` are computed once
    // here (rather than re-deriving them at each print site below) so
    // the "no feasible candidate" fallback -- reporting the classical
    // optimum instead, clearly labeled as such -- only needs writing
    // once.
    let post_selected_matches = post_selected.map(|s| s.0 == classical_bits).unwrap_or(false);
    let post_selected_bits: &[u8] = post_selected.map(|s| s.0.as_slice()).unwrap_or(&classical_bits);

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
        let swap_count = match backend.coupling_map(n) {
            Some(coupling) => route_best(&raw_circuit, &coupling).gates.iter().filter(|g| matches!(g, Gate::Swap(_, _))).count(),
            None => 0,
        };
        let lowered = lower(&raw_circuit, backend);
        let (single, two) = lowered.gate_counts();
        let cal = calibration_for(backend);
        let est_fidelity = estimate_backend_circuit_fidelity(&lowered, &cal);

        println!("  {:<12} {:>10} {:>10} {:>10} {:>15.2}%", format!("{:?}", backend), swap_count, single, two, est_fidelity * 100.0);
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
    println!("\nExecuted on {:?}; state fidelity vs. the ideal (unlowered) circuit: {:.6}", best_backend, fidelity_vs_ideal);
    println!(
        "(Expected ~1.0 here: no noise model is applied in this simulation run, so backend \
         lowering + routing should be action-preserving. The {:.2}% figure above is a \
         *published-calibration* estimate of what real hardware noise would do to this gate \
         count -- it is not applied to this run's statevector.)",
        best_fidelity * 100.0
    );

    let mut rng = Xorshift64::new(7);
    let shots = sample_shots(&backend_register, args.shots, &mut rng);
    let sampled_cost: f64 = shots
        .iter()
        .map(|&state| {
            let bits: Vec<u8> = (0..n).map(|i| ((state >> i) & 1) as u8).collect();
            qubo.cost(&bits)
        })
        .sum::<f64>()
        / shots.len() as f64;
    println!(
        "\n{} simulated shots: mean cost {:.5} (ideal E[H_cost] {:.5}, difference {:.5} -- \
         expected to shrink as shots -> infinity, this is finite-sample noise, not model error)",
        args.shots,
        sampled_cost,
        expected,
        (sampled_cost - expected).abs()
    );

    // --- 4b. Actually apply a noise model this time, then use
    //         zero-noise extrapolation. Identical logic to the
    //         portfolio example. ---
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
         section 4 above.)",
        zne_scales[0], zne_scales[1], zne_scales[2], args.noise_shots
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
    println!(
        "  Recommended assignment (QAOA, post-selected):  {}{}",
        format_assignment(post_selected_bits, &instance),
        if post_selected.is_none() { "  [no feasible QAOA candidate found -- this is the classical exact optimum]" } else { "" }
    );
    println!("  Classical exact optimum:                       {}", format_assignment(&classical_bits, &instance));
    println!("  Match: {}", post_selected_matches);
    println!("  Recommended backend: {:?} ({:.2}% estimated fidelity)", best_backend, best_fidelity * 100.0);
    println!("{}", "=".repeat(78));
    println!(
        "\nWhat's real here: district CA-storage-eligible tonnage (from reported apple \
         production x the government's own 30% CA-eligibility estimate), Lassipora's CA \
         capacity, and the real, documented capacity shortfall that motivates the 'no CA \
         storage' option. What's still estimated or placeholder: Pulwama's tonnage, the \
         Aglar/Anantnag capacity split, road distances, transport/spoilage rates, and the \
         distress-sale discount -- see kashmir_apple_instance()'s doc comment for the exact \
         line between the two. Next step (per the design note this pilot implements): take \
         this to one Kashmir aggregator, CA-store operator, or mandi committee (e.g. SIDCO \
         Lassipora, or the Aglar Industrial Estate) for the real distances, transit times, \
         and per-tonne transport/handling costs, and to J&K's Directorate of Horticulture \
         for a real per-district CA-allocation figure rather than the 0.30-of-production \
         proxy used here. Nothing else in this file needs to change to go from this \
         partially-real instance to the fully real pilot."
    );
}