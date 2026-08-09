# backend_cost_comparison

Pick the best backend for a circuit before you commit to one.

```bash
cargo run --example backend_cost_comparison
```

## What it does

Builds three standard circuits programmatically (no QASM — direct `Circuit`/`Gate` construction): a Bell pair, a 4-qubit GHZ state, and a 3-qubit QFT. For each, lowers it to every supported backend, compares native gate counts, estimates fidelity from each backend's published calibration, and recommends whichever backend produced the highest estimated fidelity.

```rust
for &backend in &BACKENDS {
    let backend_circuit: BackendCircuit = lower(circuit, backend);
    let (single, two) = backend_circuit.gate_counts();
    let calibration = backend.calibration();
    let fidelity = estimate_backend_circuit_fidelity(&backend_circuit, &calibration);
    if best.map_or(true, |(_, best_fidelity)| fidelity > best_fidelity) {
        best = Some((backend, fidelity));
    }
}
```

## Why it matters

The three circuits are chosen to have meaningfully different structure — a Bell pair is trivial for any topology, GHZ is a star pattern, QFT is dense and all-to-all — so the "best backend" answer isn't the same for all three. This is the example to reach for first when you're starting a new project and need to decide which backend to target, before you've written the rest of your circuit.

The file's own closing note is worth repeating: these are comparative estimates for choosing between supported backends, not guarantees of real hardware execution.

## Related

- [`full_pipeline`](full_pipeline.md) — the same "lower to every backend, judge by that backend's own calibration" pattern, applied to a QASM-sourced circuit instead of programmatically-built ones
- [`fidelity_scaling`](fidelity_scaling.md) — how a single backend's fidelity estimate changes as one circuit family scales up
