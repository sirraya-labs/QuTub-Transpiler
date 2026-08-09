# pipeline_end_to_end

The fullest single-file tour of the compiler: parsing, optimization, per-backend lowering and fidelity, real sampled measurement outcomes, diagrams at every stage, and a real IBM-basis QASM export.

```bash
cargo run --example pipeline_end_to_end
```

## What it does

Parses a 3-qubit source circuit containing a real `Rz`-in-the-middle-of-a-`Cx`-sandwich — the exact structure `Rzz(a,b,θ) == Cx(a,b).Rz(b,θ).Cx(a,b)` identity that `backend::IbmQSpec::push_two_qubit_zz` re-expresses in the other direction — then walks through:

1. **Source diagram** via `Diagram::from_circuit`
2. **Source-level optimization** (`optimize_ir`)
3. **Native decomposition + peephole optimization**, fidelity-estimated against Quantinuum Helios
4. **Real execution with real measurement** via `emit::run_with_measurement` — actual Born-rule-sampled outcomes, not just probabilities
5. **Multi-backend lowering**, each backend judged against its own published calibration, each actually executed with real measurement via `emit::run_backend_with_measurement`
6. **IBM-specific extras**: the lowered-circuit diagram and a real IBM-basis QASM export via `ibm_export::to_ibm_qasm` — the literal text `submit_ibm.py` would hand to Qiskit Runtime

```rust
let (_reg, clbits) = emit::run_with_measurement(&native)?;
println!("TrappedIon native measurement outcomes (by clbit): {:?}", clbits);
```

## Why it matters

This is the example to read when you want to see the entire compiler surface in one place — every other example in this repository exercises a subset of what this one does end to end. If you're deciding where to start reading the source itself, follow this file's calls in order.

## Related

- [`full_pipeline`](full_pipeline.md) — the shorter version of the same shape, without diagrams/measurement/export
- [`bell_state_end_to_end`](bell_state_end_to_end.md) — the smallest version of this same idea
- [`diagram_demo`](diagram_demo.md) — a closer look at the `Diagram` API used here
