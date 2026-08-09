# layout_comparison

Isolates *why* [`qiskit_benchmark`](qiskit_benchmark.md)'s numbers lagged Qiskit's `transpile()` on nearest-neighbor-structured circuits — even once both sides routed against the identical heavy-hex coupling map.

```bash
cargo run --example layout_comparison
```

## The root cause

`backend::lower` always calls `route::route`, which starts every circuit from the trivial identity layout (logical qubit *i* → physical qubit *i*) and routes each two-qubit gate's shortest path in isolation. The codebase already ships a second, smarter pass — `route::route_lookahead` — which starts from a weighted placement heuristic (`route::choose_initial_layout`) and scores candidate SWAPs against a lookahead front layer, SABRE-style. Nothing in `backend::lower` calls it.

## What it does

Runs the same three circuits `qiskit_benchmark.rs` used (GHZ's star pattern, a layered hardware-efficient ansatz, a layered-random circuit) plus `routing_demo`'s all-to-all QFT-style stress case, through *both* routing passes against the same `CouplingMap::heavy_hex_for` topology, and reports the SWAP count and depth each produces — plus the same correctness check `routing_demo` and `verify_equivalence` use: state fidelity 1.0 against the unrouted reference for *both* passes, confirming the smarter pass isn't trading correctness for a smaller SWAP count.

```rust
let naive = route(circuit, &coupling);
let smart = route_lookahead(circuit, &coupling);
let fid_naive = reference.fidelity(&run_unitary(&naive)).expect("fidelity");
let fid_smart = reference.fidelity(&run_unitary(&smart)).expect("fidelity");
assert!((fid_naive - 1.0).abs() < 1e-9 && (fid_smart - 1.0).abs() < 1e-9, ...);
```

`route_lookahead` is documented (and regression-tested: `smart <= naive` SWAP count) to never do *worse* than plain `route`. This example measures *how much* better, on circuits chosen for a different reason entirely — matching Qiskit's benchmark set — rather than cherry-picked to flatter one pass over the other.

## A second experiment: why does the alternating-parity circuit regress?

The file also includes `layered_random_fixed_parity`, built specifically to test one hypothesis about a SWAP-count regression the example found in `route_lookahead` on the *alternating*-parity version of `layered_random`: `choose_initial_layout` picks one static layout from aggregated, whole-circuit interaction weights, with no notion of *when* an interaction happens. If a circuit's "good" adjacency structure shifts over time (as the alternating ladder's does), no single static layout may suit it well, and `route_lookahead` pays real SWAP cost moving into and out of that layout for little benefit. Holding parity fixed removes the time-varying structure — if the regression shrinks or disappears, that supports the hypothesis; if it doesn't, something else is the cause. This is the example actually testing its own explanation, not just asserting it.

## Why it matters

The example ends with a concrete, actionable conclusion: since every SWAP costs 3 native two-qubit gates once lowered, wiring `route_lookahead` into `backend::lower`'s IbmQ/Rigetti path is a plumbing change, not a research problem — the smarter pass already exists and is already tested.

## Related

- [`qiskit_benchmark`](qiskit_benchmark.md) — the external comparison this example explains the gap in
- [`routing_demo`](routing_demo.md) — the same fidelity-preservation check, focused on topology rather than algorithm choice
