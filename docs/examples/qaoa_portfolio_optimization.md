# qaoa_portfolio_optimization

QAOA solving a real (if small) combinatorial optimization problem — Markowitz portfolio selection — checked against brute-force enumeration, then run through the same real-noise-and-mitigation pipeline as the VQE example.

```bash
cargo run --release --example qaoa_portfolio_optimization
cargo run --release --example qaoa_portfolio_optimization -- --p-layers 2 --shots 4096
```

## The problem

A synthetic 8-asset basket with a genuine 3-member sector (Tech) so a "max 2 per sector" diversification constraint is actually binding, not decorative. The goal: pick a fixed-size subset of assets minimizing `risk_aversion × variance − expected_return`, subject to (a) an exact budget (pick exactly *k* assets) and (b) a per-sector cap.

```rust
fn synthetic_basket() -> AssetBasket { /* 8 assets, 5 sectors */ }
```

Covariance is generated from a documented rule rather than hand-typed — same-sector assets correlated, one asset (Gold) anti-correlated with everything as a hedge — so the structure is auditable rather than arbitrary.

## From portfolio to Ising Hamiltonian

Both constraints are folded into the objective as quadratic penalties via the standard `penalty × (Σxᵢ − k)²` reduction (Lucas 2014, "Ising formulations of many NP problems") — applied once to the whole portfolio for the budget, and once per over-capacity sector for diversification:

```rust
Self::add_cardinality_penalty(&mut linear, &mut quadratic, &(0..n).collect::<Vec<_>>(), budget, penalty);
```

The resulting QUBO is converted to an Ising Hamiltonian via the standard `xᵢ = (1 − zᵢ)/2` substitution, giving `Σ hᵢZᵢ + Σ Jᵢⱼ ZᵢZⱼ` — the form the QAOA cost unitary is built from.

## The QAOA circuit and angle optimization

A standard *p*-layer QAOA ansatz: uniform superposition, then per layer one cost unitary (`Rz`/`Rzz`, from the Ising Hamiltonian above) and one mixer unitary (`Rx` on every qubit). Angles are found via a dependency-free multi-start, coarse-to-fine grid search — the same outer-loop problem a production QAOA implementation would hand to COBYLA or SPSA, just solved here without pulling in an external optimizer crate. Every trial is a real ideal-simulator evaluation, no fabricated numbers.

```rust
fn qaoa_circuit(n: usize, h: &[f64], j_terms: &[(usize, usize, f64)], gammas: &[f64], betas: &[f64]) -> Circuit
```

## Verification against a known-exact baseline

With 8 assets, brute-force enumeration over all 256 bitstrings is exact and cheap — `Qubo::brute_force_optimal` is the classical ground truth every quantum result is checked against, and `is_feasible` independently verifies the budget/sector constraints directly (not derived from QUBO cost), confirming what the penalty terms are actually supposed to enforce.

## Real noise and mitigation

Once the ideal-simulator angles are found, the same `CircuitExecutor` / `NoisyBackendExecutor` / weighted zero-noise-extrapolation pattern from [`vqe_h2_ground_state`](vqe_h2_ground_state.md) applies: route to the best backend, inject real Monte-Carlo Pauli-kick noise at several amplified scales, and extrapolate back to the zero-noise limit.

## What this example is careful not to claim

The file is explicit that this is a correctness-and-pipeline demonstration, not a quantum-advantage claim: 8 assets is brute-forceable in microseconds classically, and the asset data is synthetic — not investment advice.

## Related

- [`vqe_h2_ground_state`](vqe_h2_ground_state.md) — the file this one's noise/ZNE machinery is adapted from; read that one first if you haven't already
- [`trotter_ising_dynamics`](trotter_ising_dynamics.md) — a third application of the same pattern, for physics simulation
