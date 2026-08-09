# trotter_ising_dynamics

Trotterized time evolution of a transverse-field Ising spin chain — the same experiment shape as IBM's 2023 "utility" demonstration (Kim et al., *Nature* 618, 500), verified against an independent classical integration of the actual Schrödinger equation.

```bash
cargo run --release --example trotter_ising_dynamics
cargo run --release --example trotter_ising_dynamics -- --trotter-steps 16 --noise-shots 150000
```

## The model

An open-boundary chain, `H = −J Σ ZᵢZᵢ₊₁ − h Σ Xᵢ`, evolved from `|00...0⟩` to time `T`. The reported observable is average single-site magnetization `⟨Z⟩_avg` at time `T` — a real, physically meaningful quantity, not a synthetic benchmark number.

## The independent classical reference

A from-scratch 4th-order Runge-Kutta integrator solves `dψ/dt = −iHψ` directly on the full statevector, with `H` applied via its sparse structure (`ZᵢZᵢ₊₁` is diagonal, `Xᵢ` flips one bit) — no circuit, no matrix ever built or diagonalized. This plays the same role the closed-form diagonalization played in [`vqe_h2_ground_state`](vqe_h2_ground_state.md): ground truth computed entirely independently of the thing being tested.

## A convergence check that's also a self-check

The example sweeps Trotter step count on the *ideal* simulator and confirms the circuit's result converges toward the RK4 reference as step count increases. This does double duty: it demonstrates the algorithm, and it validates the assumed `Rzz`/`Rx` gate-angle convention the Trotter circuit is built from — if the convention were wrong, this convergence simply wouldn't happen, which is a much stronger check than asserting the convention and hoping.

## Separating algorithmic error from noise error

The total gap between a NISQ result and the true continuum answer has two independent sources, and zero-noise extrapolation can only ever fix one of them:

1. **Trotter (algorithmic) error** — from using a finite step count instead of true continuous-time evolution. Present identically on the ideal simulator; ZNE has no way to touch it.
2. **Noise-induced error** — how far the noisy-hardware result deviates from what the *same circuit* gives on a perfect simulator.

The example reports both separately rather than collapsing them into one comparison, because comparing a mitigated NISQ result only against the true continuum answer unfairly penalizes ZNE for a step-count choice it has no way to correct:

```rust
let noise_gap_raw = (scale_mean[0] - device_ideal_avg).abs();
let noise_gap_mitigated = (mitigated - device_ideal_avg).abs();
```

## Finding the actual sweet spot, not guessing at one

Fewer Trotter steps means more algorithmic error but a shallower (less noisy) circuit; more steps means less algorithmic error but a deeper circuit that's harder for a fixed shot budget to fully denoise. The example includes a step-count trade-off sweep that empirically finds the total-error-minimizing configuration across several candidates, rather than picking one by intuition:

```rust
if total_err < best_sweep_total_error {
    best_sweep_total_error = total_err;
    best_sweep_steps = steps;
}
```

## Why it matters

This is the example that most directly mirrors a real published hardware result's methodology — independent classical verification, a self-checking convergence sweep, an honest decomposition of error sources, and an empirically-found operating point instead of an assumed one. If you're building your own physics-simulation demo on top of this crate, this file's structure (not just its physics) is the template worth copying.

## Related

- [`vqe_h2_ground_state`](vqe_h2_ground_state.md) — the noise/ZNE machinery this file adapts, applied to a ground-state problem instead of dynamics
- [`qaoa_portfolio_optimization`](qaoa_portfolio_optimization.md) — the same pattern again, for combinatorial optimization
