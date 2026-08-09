# full_pipeline

QASM in, real optimization, real per-backend lowering, real fidelity budgets, real execution — across all three backends, in one file.

```bash
cargo run --example full_pipeline
```

## What it does

Parses a 3-qubit QASM program (`h`, `cx`, `t`, `ryy`, `swap`, `cp` — a genuine mix, not a toy circuit), runs source-level optimization, then walks two paths:

**TrappedIon-specific path:** decompose to native `{Rz, Ry, Rzz}`, peephole-optimize, estimate fidelity against Quantinuum Helios's published calibration (the specific figures that gate set describes), execute on the simulator, print the resulting probability distribution.

**Multi-backend path:** for each of `TrappedIon`, `IbmQ`, `Rigetti`, lower the *same* source circuit to that backend's own native gate set, estimate fidelity against *that backend's own* published calibration (not one backend's numbers reused for another's gate counts), and actually execute it — not just gate-count it.

```rust
for backend in [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti] {
    let bc: BackendCircuit = lower(&circuit, backend);
    let backend_cal = backend.calibration();
    let backend_fidelity = estimate_backend_circuit_fidelity(&bc, &backend_cal);
    let backend_reg = emit::run_backend(&bc)?;
    print_distribution(&backend_reg);
}
```

## Why it matters

The detail worth noticing: every backend is judged against its *own* calibration data. It would be easy (and wrong) to compute one fidelity number and apply it across backends — this example is explicit that TrappedIon is judged against Quantinuum Helios, IbmQ against IBM Heron r2, Rigetti against Rigetti Ankaa-3, because those are the devices those published figures actually describe.

## Related

- [`pipeline_end_to_end`](pipeline_end_to_end.md) — same shape, plus circuit diagrams, real sampled measurements, and IBM QASM export
- [`backend_cost_comparison`](backend_cost_comparison.md) — the comparison-focused version of this same multi-backend loop
