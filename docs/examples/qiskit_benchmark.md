# qiskit_benchmark

The real external benchmark: this crate's routing and lowering, measured head-to-head against Qiskit's own `transpile()`, on the same circuits, targeting the same real IBM basis gate set and the same real coupling map.

```bash
cargo run --example qiskit_benchmark        # writes QASM + coupling files
python3 qiskit_transpile_compare.py         # runs Qiskit's side, requires: pip install qiskit
```

## What it does

Builds 14 benchmark circuits chosen to stress-test different parts of the router and optimizer, each for a genuine, documented reason rather than arbitrarily:

| Circuit | Why it's included |
|---|---|
| `ghz_10`, `ghz_16` | Star pattern — every `Cx` shares qubit 0 |
| `ansatz_6q_3layer` | Hardware-efficient ansatz — the shape most VQE/QAOA circuits actually have |
| `layered_random_8q_4round` | No structural assumptions the router could exploit |
| `bernstein_vazirani_10` | Every two-qubit gate shares the *same* target (the ancilla) — a hub/star pattern that turns out to be a genuine stress case for a per-gate greedy router, distinct from QFT's long-range cascade or QAOA's arbitrary-graph edges |
| `qaoa_maxcut_8q_p2`, `qaoa_maxcut_12q_p3` | Generic non-adjacent edge pairs — exercises the general-purpose routers rather than any dedicated fast path |
| `trotter_ising_10q_6step`, `trotter_ising_16q_10step` | Purely nearest-neighbor physics workload — should route with few or zero SWAPs on any topology with a Hamiltonian path, isolating "does the optimizer handle a realistic circuit well" from "does routing handle long-range interactions well" |
| `qpe_6counting`, `qpe_10counting` | Quantum Phase Estimation, deliberately built in *reverse* cascade order so it does **not** hit `route_qft`'s dedicated fast path — a second, independent data point on the general routers |
| `dynamic_midcircuit_measure_10q` | A genuine mid-circuit measurement (not just a final one) — exercises `Measure` support being routed and optimized *through*, not just around |
| `qft_10`, `qft_16` | The QFT's dedicated fast path (`route_qft`) |
| `long_range_random_20q_60gate` | Unstructured long-range stress case at larger width |

For each circuit, it writes portable QASM 2.0 and the exact `CouplingMap::heavy_hex_for(n)` edge list to `qiskit_benchmark_qasm/`, routes with `route::route_best` against that same coupling map, lowers to IBM's native basis, and reports source gate count, IBM-basis depth, native 1q/2q gate counts, estimated fidelity, and SWAP count.

## The restoration-tax analysis

A second table breaks each circuit's SWAP count into two categories via `route::restoration_swap_count`: **routing** SWAPs (load-bearing — actually needed to satisfy connectivity mid-circuit) and **restoration** SWAPs (a trailing block that puts every logical qubit back on its original physical wire after routing is done). It then compares against `route::route_best_no_restore`, which skips that trailing restoration — valid whenever a circuit's result is read off its `Measure` outputs rather than its final qubit layout — and reports the resulting fidelity delta in percentage points.

```rust
let (routing_swaps, restoration_swaps) = route::restoration_swap_count(&routed);
let no_restore_routed = route::route_best_no_restore(circuit, &coupling);
```

This answers a genuinely useful question: for a given circuit, how much of its SWAP overhead is a mandatory connectivity cost, and how much is a bookkeeping cost you can skip if you don't need the final layout restored?

## The Python side

`qiskit_transpile_compare.py` reads the same QASM and coupling files, runs each circuit through Qiskit's `transpile()` targeting the identical `{rz, sx, x, cx}` basis and the identical heavy-hex coupling map at `optimization_level=3`, and prints a directly comparable table. See the [Companion scripts](../examples.md#companion-scripts) section for the full script reference.

## Why it matters

The coupling-map matching between both sides is what makes this a fair comparison rather than a misleading one — without it, Qiskit would be solving a strictly easier unconstrained problem. If you only run one benchmark to gauge where this crate currently stands against an established transpiler, this is the one — and pair it with [`layout_comparison`](layout_comparison.md) to understand *why* any gap exists, not just that one does.

## Related

- [`layout_comparison`](layout_comparison.md) — explains the specific, fixable cause of this crate's current SWAP-count gap on nearest-neighbor circuits
- [`routing_demo`](routing_demo.md) — the same heavy-hex coupling map, focused on making SWAP cost visible rather than external comparison
