# vqe_h2_ground_state

A complete Variational Quantum Eigensolver run — not a toy demonstration. Finds the ground-state energy of molecular hydrogen, checks the answer against an exact closed-form solution, then shows exactly what real hardware noise does to that answer and how much of it zero-noise extrapolation can claw back.

```bash
cargo run --release --example vqe_h2_ground_state
cargo run --release --example vqe_h2_ground_state -- --noise-shots 50000
```

Use `--release` — this runs a real classical optimization loop and (at the default shot count) tens of thousands of Monte-Carlo noise trajectories.

## The problem

H₂ at 0.75 Å bond length, mapped to a 2-qubit qubit Hamiltonian via a minimal STO-3G-derived basis. Small enough that the exact ground-state energy is known from closed-form diagonalization — so "correct" is a known number, not an estimate the example also has to trust.

## The five stages

1. **Exact reference** — closed-form diagonalization of the 2-qubit Hamiltonian, independent of anything the circuit does.
2. **VQE against the ideal simulator** — a parameterized ansatz (3 layers, 21 parameters) optimized via coordinate descent until its measured energy approaches the exact value. This validates the *algorithm*: if the ansatz can't find the right answer on a perfect simulator, no amount of hardware-noise mitigation downstream will fix that.
3. **Backend comparison** — the optimized circuit routed and lowered against all four backends' real coupling maps, each scored by estimated fidelity from its own routed gate counts, not a generic spec number.
4. **Real Monte-Carlo noise** — the winning backend's circuit actually run with simulated noise (Pauli kicks injected per-gate at a rate backed out of the estimated fidelity), amplified across five scale points (`1.0` to `3.0`).
5. **Zero-noise extrapolation** — a *weighted* least-squares fit (points weighted by their own measured precision) back to the zero-noise limit, returning both an estimate and a propagated uncertainty on that estimate.

## Why the ZNE fit is weighted, not naive

An unweighted linear fit lets a noisy point at one scale distort the extrapolated answer as much as a precise point at another. The weighted fit here also yields a closed-form standard error on the mitigated result — which matters, because it lets the example distinguish "mitigation genuinely improved the estimate" from "it happened to land closer this run by chance":

```rust
let improvement = raw_gap - mitigated_gap;
let improvement_significant = improvement.abs() > mitigated_stderr;
```

## Reading the output

The headline comparison is against the field-standard **chemical accuracy** threshold (< 0.0016 Hartree, ~1 kcal/mol — the same bar VQE hardware demonstrations have been judged against since the original Google/IBM papers). At a reasonable noise level and shot count, the typical pattern is: the ideal simulator passes comfortably, raw noisy execution fails, and the ZNE-mitigated result passes again — with the improvement's statistical significance reported explicitly, not just claimed.

## Why it matters

This is the example to point to for "does this crate's noise-mitigation pipeline actually work, or is it a demo dressed up as one?" Every claim in the output is backed by a number you could re-derive: the exact answer is closed-form, the mitigation's uncertainty is propagated from the actual fit, and the significance check means a "raw fails / mitigated passes" headline only appears when it's real.

## Related

- [`qaoa_portfolio_optimization`](qaoa_portfolio_optimization.md) — the same real-noise-plus-ZNE pipeline, applied to a combinatorial optimization problem instead of chemistry
- [`trotter_ising_dynamics`](trotter_ising_dynamics.md) — the same pipeline again, for a dynamics simulation, with an added error-decomposition step separating algorithmic error from noise
