# bell_state_end_to_end

The smallest complete run through the real pipeline — the right first example to run.

```bash
cargo run --example bell_state_end_to_end
```

## What it does

Parses a two-line Bell-state OpenQASM 2.0 program, runs it through `optimize_ir`, lowers it to IBM's native gate set, and exports real IBM-basis QASM via `to_ibm_qasm`. Separately, it generates a reference measurement distribution by running the compiled circuit on the local simulator 4,096 times — each "shot" re-runs the circuit from a fresh `|00⟩` state, since `QuantumRegister`'s measurement collapses the state and there's no batched-shots primitive on the register itself.

```rust
let circuit = qasm::parse(source).expect("parse failed");
let circuit = optimize_ir(&circuit);
let backend_circuit = lower(&circuit, Backend::IbmQ);

let qasm_text = to_ibm_qasm(&backend_circuit, "bell_state").expect("IBM export failed");
fs::write("bell.qasm", &qasm_text).expect("failed to write bell.qasm");
```

## Output files

| File | What it is |
|---|---|
| `bell.qasm` | Real IBM-basis QASM (`rz`/`sx`/`x`/`cx`), ready to hand to Qiskit or IBM Quantum Platform |
| `bell_reference_counts.json` | The simulator's 4,096-shot reference distribution, hand-serialized to JSON (no `serde_json` dependency needed for this one file) |

## Why it matters

This is the exact handoff point the companion script [`submit_ibm.py`](../examples.md#submit_ibmpy) expects: run this example first, then

```bash
python3 submit_ibm.py --qasm bell.qasm --real --backend <name> \
  --compare bell_reference_counts.json
```

to submit the same circuit to real IBM Quantum hardware and see how far the real result drifts from the ideal simulator's distribution (measured as total variation distance).

## Related

- [`pipeline_end_to_end`](pipeline_end_to_end.md) — the fuller version of this same pipeline shape, across all three backends, with diagrams
- [`gate_cheatsheet`](gate_cheatsheet.md) — what the `H` and `CX` gates in this circuit actually cost once decomposed
