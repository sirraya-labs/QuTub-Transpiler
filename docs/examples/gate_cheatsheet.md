# gate_cheatsheet

What does each source-level gate actually cost on real hardware? This example answers that directly.

```bash
cargo run --example gate_cheatsheet
```

## What it does

For every gate the compiler's source-level `Gate` enum supports, builds a one-gate circuit, decomposes it to the native `{Rz, Ry, Rzz}` set, runs it through the peephole optimizer, and prints the resulting gate sequence — both the raw decomposition and the optimized one, with counts.

```rust
fn show(label: &str, gate: Gate, num_qubits: usize) {
    let mut circuit = Circuit::new(num_qubits);
    circuit.push(gate);
    let raw = decompose(&circuit);
    let opt = optimize(&raw);
    let (single, two) = opt.gate_counts();
    println!("{label:<18} raw={:>2} gates  optimized={:>2} gates  ({single} single-qubit, {two} two-qubit)", ...);
}
```

## What you'll see

**Single-qubit gates → `{Rz, Ry}`:** `H`, `X`, `Y`, `Z`, `S`, `T`, `Rx` all decompose to some sequence of `Rz`/`Ry`. `Ry` and `Rz` themselves are already native — zero-cost.

**Two-qubit gates → `{Rz, Ry, Rzz}`:** `Cx`, `Cz`, `Swap`, `Rxx`, `Ryy`, `Cp` all cost exactly 1 native two-qubit gate (`Rzz`) plus some single-qubit dressing — except `Swap`, which costs 3. `Rzz` itself is already native.

## Why it matters

Two facts worth internalizing from this table:

1. **Not all gates are equal cost**, even within "one gate" — `H` costs 2 single-qubit gates where `Ry`/`Rz` cost 0 (already native). A circuit built entirely from native rotations compiles smaller than the same logical operation expressed via `H`/`X`/`Y`/`Z`.
2. **`Swap` is expensive** — 3 native two-qubit gates, not 1. This is *why* routing overhead ([`routing_demo`](routing_demo.md)) matters so much: every `Swap` the router inserts to satisfy connectivity costs 3x what a "free" two-qubit gate would.

## Related

- [`fidelity_scaling`](fidelity_scaling.md) — shows what this gate-cost accounting means at scale (fidelity vs. gate count)
- [`routing_demo`](routing_demo.md) — why `Swap`'s 3x cost makes connectivity the real bottleneck, not gate translation
