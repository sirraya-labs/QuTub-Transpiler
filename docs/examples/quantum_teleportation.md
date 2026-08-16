# quantum_teleportation

Quantum teleportation — Bennett, Brassard, Crépeau, Jozsa, Peres & Wootters, *Phys. Rev. Lett.* 70, 1895 (1993) — run through this crate's real compiler pipeline, verified against the real simulator's own density-matrix machinery across six input states and cross-checked by an independent Bloch-vector calculation.

```bash
cargo run --release --example quantum_teleportation
```

## The protocol

Alice holds one qubit in an unknown state `|psi> = a|0> + b|1>`. She and Bob each hold one half of a pre-shared Bell pair. Alice performs a two-qubit entangling measurement on her message qubit and her half of the pair, sends the two classical bits that measurement produces to Bob, and Bob applies one of four fixed corrections (`I`, `X`, `Z`, or `XZ`) depending on which bits arrive. `|psi>` — not a copy, since Alice's own qubit is destroyed by her measurement — ends up on Bob's qubit.

**Not faster-than-light.** Bob's qubit isn't `|psi>` until the classical bits physically arrive; before that it's maximally mixed on its own. The outcome-distribution check below verifies this numerically: which of the four outcomes occurs is independent of `psi`, so the classical bits alone carry zero information about the teleported state.

## How this differs from a toy simulation

Two things distinguish this from calling `sirraya_qutub` directly (see that crate's own `examples/teleportation.rs`, which this follows for the measurement/correction/verification structure):

1. **The entangling half goes through the real pipeline** — `ir_optimize::optimize`, `route::route_best` against an actual backend coupling map, `backend::lower` + `fidelity::estimate_backend_circuit_fidelity` for the backend comparison, and native decomposition — not a hand-written 3-gate sequence run straight against the simulator.
2. **Correctness is checked against the real simulator's own density-matrix machinery** (`to_density_matrix`, `partial_trace`, `DensityMatrix::fidelity`), cross-checked against an independent Bloch-vector calculation, across six different input states and many repeated trials per state — not asserted once for a single hardcoded case.

## An honest architectural gap

`ir::Circuit`/`Gate` has no classical-control construct today — `Gate::Measure` writes a classical bit, but there's no "apply this gate only if that bit is 1" gate. That's a real, open gap, not something this example works around silently. The classically-conditioned half of the protocol — measurement and the resulting correction — is applied directly against the `QuantumRegister` `emit::run` returns, via `measure_single_qubit` and `apply_pauli_x`/`apply_pauli_z`, exactly like `sirraya_qutub`'s own reference example does. This mirrors how real hardware actually splits the problem: static gate compilation (this crate's pipeline) is a separate concern from real-time classical feed-forward control electronics (what this step stands in for).

Because every routing pass `route_best` can return (`route`, `route_lookahead`, `route_sabre`, `route_qft`) calls `restore_identity_mapping` before returning, physical qubit `i` in the routed circuit always means logical qubit `i` again — so it's safe to call `register.measure_single_qubit(0)`/`(1)` and correct qubit `2` directly after execution, without separately tracking a logical/physical remapping.

## What gets checked, and how

The example runs in three stages:

1. **Backend comparison.** The entangling circuit (built for the `|i>` state) is lowered and fidelity-estimated against all four backends, and the highest-estimated-fidelity backend is picked for the noisy run in stage 3 — same pattern as [`backend_cost_comparison`](backend_cost_comparison.md).
2. **Ideal-simulator verification.** 200 trials each for six input states (`|0>`, `|1>`, `|+>`, `|->`, `|i>`, and an arbitrary `Ry(0.7) Rz(1.3) |0>`). For every trial, fidelity is computed two independent ways — `DensityMatrix::fidelity` and the Bloch-vector inner-product identity — and asserted to agree to `1e-9`; a disagreement would mean a bug in one of the two code paths, since both are exact for a pure target state. The four possible classical outcomes (`00`/`01`/`10`/`11`) are also tallied per state: recovery at ~100% fidelity under every outcome, combined with an outcome split that stays ~25/25/25/25 regardless of which state was sent, is what rules out both a lucky single case and any hidden signaling channel.
3. **Realistic noise + zero-noise extrapolation.** The recommended backend from stage 1 runs the `|i>` state through the same `NoisyBackendExecutor`/ZNE machinery as [`trotter_ising_dynamics`](trotter_ising_dynamics.md) and [`vqe_h2_ground_state`](vqe_h2_ground_state.md) — route through the real coupling map, inject per-gate Pauli kicks at a rate backed out of the backend's estimated fidelity, sweep five noise scales, and fit a weighted least-squares extrapolation back to zero noise, reporting both the raw and mitigated fidelity with propagated standard error.

Genuinely random measurement outcomes come from the real simulator's own (unseeded, system-entropy) RNG; the noise-injection model uses a separately seeded xorshift64 PRNG for reproducibility. The two random sources are kept deliberately distinct.

## Why it matters

This is the example that most directly demonstrates the no-cloning and no-signaling properties of teleportation numerically rather than just asserting them: the density-matrix/Bloch-vector cross-check on every trial catches a broken correction step immediately, and the outcome-independence tally is a real (if small) statistical test that the classical bits carry no information about `psi` on their own — the same kind of two-independent-routes verification [`trotter_ising_dynamics`](trotter_ising_dynamics.md) uses against its RK4 reference.

## Related

- [`vqe_h2_ground_state`](vqe_h2_ground_state.md) — the noise/ZNE machinery this file adapts, applied to a ground-state problem instead of teleportation
- [`trotter_ising_dynamics`](trotter_ising_dynamics.md) — the same `CircuitExecutor` / `NoisyBackendExecutor` / `zero_noise_extrapolate` pattern, applied to Hamiltonian dynamics
- [`backend_cost_comparison`](backend_cost_comparison.md) — the fidelity-estimate backend comparison this example's step 1 reuses
