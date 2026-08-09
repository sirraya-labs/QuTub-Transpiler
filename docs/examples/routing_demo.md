# routing_demo

What actually kills fidelity on real hardware isn't gate translation — it's connectivity. This example makes that cost visible and checks it's paid correctly.

```bash
cargo run --example routing_demo
```

## What it does

Builds an 8-qubit all-to-all-entangling circuit (`H` on every qubit, then a controlled-phase between *every* pair — the core structure of a QFT, and about as connectivity-hostile as a circuit gets) and routes it against three real topologies:

- `CouplingMap::linear` — a worst-case chain, for contrast
- `CouplingMap::heavy_hex_for` — IBM's actual Eagle/Heron lattice family
- `CouplingMap::square_grid_for` — Rigetti Ankaa-class hardware

For each, it reports SWAP count, resulting depth, and — the check that actually matters — **state fidelity 1.0** between the routed circuit's output and the unrouted reference's output, proving `route::route` changes *how* the circuit runs without changing *what* it computes.

```rust
let fidelity = reference.fidelity(&routed_reg).expect("fidelity");
```

It then runs the full pipeline (`route → backend::lower → fidelity budget`) on IbmQ and Rigetti, showing routing overhead as what it actually costs: extra native two-qubit gates eating into the fidelity estimate (recall from [`gate_cheatsheet`](gate_cheatsheet.md) that every `Swap` is 3 native two-qubit gates once lowered).

## A genuine finding, not a foregone conclusion

The example runs a second comparison at 12 qubits using `CouplingMap::heavy_hex_grid(1, 1)` — a *full*, untruncated heavy-hex unit cell — rather than `heavy_hex_for(8)`'s BFS-truncated fragment. The point: an 8-qubit heavy-hex fragment isn't guaranteed to route better than a plain line. A sparse, degree-≤3 fragment cut off mid-lattice can have worse average pairwise distance than a straight chain of the same length. The example checks this directly rather than assuming heavy-hex always wins, and reports which way it actually went.

## Why it matters

The lesson the example closes on: routing quality depends on the *real, specific* topology at the *actual* qubit count you're compiling for — not on which topology family sounds better connected in the abstract.

## Related

- [`layout_comparison`](layout_comparison.md) — compares two different *routing algorithms* against the same topology, rather than one algorithm against different topologies
- [`gate_cheatsheet`](gate_cheatsheet.md) — why a `Swap` costing 3 native gates is what makes routing overhead so expensive
- [`verify_equivalence`](verify_equivalence.md) — the same fidelity-based correctness check, run over many more circuits
