# fidelity_scaling

It's gate count, not qubit count, that drives your fidelity budget down. This example makes that concrete.

```bash
cargo run --example fidelity_scaling
```

## What it does

Builds GHZ-state-preparation circuits (`H` on qubit 0, then a `Cx` ladder out to every other qubit) at widths `[2, 4, 8, 16, 32, 64, 98]`, compiles each to the native gate set, and prints source gate count, native `(1q, 2q)` counts, optimized gate count, and estimated fidelity on Quantinuum Helios.

```rust
for num_qubits in [2usize, 4, 8, 16, 32, 64, 98] {
    let circuit = ghz_circuit(num_qubits);
    let native = optimize(&decompose(&circuit));
    let fidelity = estimate_circuit_fidelity(&native, &cal);
    // ...
}
```

## Why 98 qubits specifically

98 matches Quantinuum Helios's own qubit count. A full-width GHZ state at that size is close to a worst case for the device: *n*−1 sequential two-qubit gates sit on the critical path, so the fidelity estimate at the top of the table is a genuine "how bad can it get on this exact hardware" number, not an arbitrary large-*n* extrapolation.

## Why it matters

The pattern across the table is the point: fidelity doesn't degrade because qubit *count* went up — it degrades because *gate* count went up, and those two only track each other because this particular circuit's gate count happens to scale linearly with width. A compiler that avoids unnecessary gates (the peephole `optimize` pass, run here on every row) matters more, not less, as circuits grow, because every gate it removes is one less multiplicative factor in the fidelity estimate.

## Related

- [`gate_cheatsheet`](gate_cheatsheet.md) — the per-gate cost accounting this example's fidelity numbers are built from
- [`backend_cost_comparison`](backend_cost_comparison.md) — the same fidelity-estimation machinery, comparing backends instead of scaling one circuit
