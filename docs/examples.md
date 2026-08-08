# Examples

The repository ships fourteen runnable examples in `examples/`. Every one of them is real, working code against the actual compiler — nothing here is pseudocode or a notebook stand-in. Each file also carries its own detailed module-level doc comment explaining the specific mechanics and design decisions behind it; this page is the map, not a replacement for that.

Run any example with:

```bash
cargo run --release --example <name>
```

(Use `--release` for anything that runs an optimization loop or a Monte-Carlo noise sweep — several examples below are 10-50x slower in debug builds.)

## I want to...

| ...do this | Start with |
|---|---|
| See the whole compiler work end to end for the first time | [`bell_state_end_to_end`](#bell_state_end_to_end) |
| Understand what a specific gate actually costs on real hardware | [`gate_cheatsheet`](#gate_cheatsheet) |
| See a circuit change shape as it moves through compilation | [`diagram_demo`](#diagram_demo) |
| Pick the best backend for a circuit I'm building | [`backend_cost_comparison`](#backend_cost_comparison) |
| Understand why connectivity, not gate translation, is what really costs fidelity | [`routing_demo`](#routing_demo) |
| Know whether the compiler's routing is actually competitive | [`layout_comparison`](#layout_comparison) or [`qiskit_benchmark`](#qiskit_benchmark) |
| Convince myself a compiler rewrite didn't silently change my circuit | [`verify_equivalence`](#verify_equivalence) |
| See a real variational algorithm, with real hardware noise and real error mitigation | [`vqe_h2_ground_state`](#vqe_h2_ground_state) or [`qaoa_portfolio_optimization`](#qaoa_portfolio_optimization) |
| See a physics simulation benchmarked against an independent classical reference | [`trotter_ising_dynamics`](#trotter_ising_dynamics) |

---

## Start here

Three short examples that between them touch every stage of the compiler at least once. Read these before anything else.

### `bell_state_end_to_end`

The smallest complete pipeline run: parse a Bell-state QASM program, optimize it, lower it to IBM's native gate set, export real IBM-basis QASM, and generate a reference measurement distribution from the local simulator. Produces two real files (`bell.qasm`, `bell_reference_counts.json`) meant to be handed straight to `submit_ibm.py` to compare a simulated result against an actual hardware run.

**What you'll learn:** the shape every other example follows — `parse → optimize → lower → export/execute` — in its smallest possible form.

```bash
cargo run --example bell_state_end_to_end
```

### `gate_cheatsheet`

For every source-level gate the compiler understands, prints the exact native `{Rz, Ry, Rzz}` sequence it decomposes to, before and after the peephole optimizer. Answers the question "what does this gate actually cost?" directly — e.g. a CNOT is always 1 two-qubit gate, a bare `Ry` is free (already native), `H` costs 2 single-qubit gates.

**What you'll learn:** the real hardware cost of every gate in the source language, at a glance.

```bash
cargo run --example gate_cheatsheet
```

### `diagram_demo`

Renders the same small circuit at three different compilation stages — source IR, native `{Rz, Ry, Rzz}`, and IBM-lowered — as ASCII art, plus an SVG export.

**What you'll learn:** what compilation actually *does* to a circuit's shape, visually, instead of just as gate counts.

```bash
cargo run --example diagram_demo
```

---

## Compiler pipeline, end to end

Two examples that run the complete pipeline against a real multi-gate circuit across every supported backend, differing mainly in how much of the output (diagrams, real sampled measurements) they surface.

### `full_pipeline`

QASM in, source-level optimization, native decomposition + fidelity estimate on the TrappedIon path, then the same circuit lowered and *actually executed* (not just gate-counted) on all three backends — each judged against its own published calibration data, not one backend's numbers reused for another's gate counts.

```bash
cargo run --example full_pipeline
```

### `pipeline_end_to_end`

The fuller version of `full_pipeline`: adds circuit diagrams at every stage, real Born-rule-sampled measurement outcomes (not just probability distributions), and a real IBM-basis QASM export via `ibm_export` — the actual text `submit_ibm.py` would hand to Qiskit Runtime.

**What you'll learn (both):** how one logical circuit turns into three different real, hardware-targeted circuits, each with its own fidelity budget, and how to get from a `Circuit` to something you could actually submit.

```bash
cargo run --example pipeline_end_to_end
```

---

## Hardware, routing & topology

What actually costs fidelity on real hardware is connectivity, not gate translation. These three examples make that concrete and measurable.

### `routing_demo`

Builds a deliberately connectivity-hostile circuit (all-to-all entangling, QFT-style) and routes it against IBM's real heavy-hex lattice, Rigetti's real square grid, and a worst-case linear chain, reporting SWAP count, depth, and — critically — a correctness check that routing never changes the circuit's actual output (state fidelity 1.0 against the unrouted reference). Includes a genuine finding, not just a demo: an 8-qubit heavy-hex *fragment* doesn't automatically beat a plain line, and the example checks that directly rather than assuming heavy-hex always wins.

```bash
cargo run --example routing_demo
```

### `layout_comparison`

Head-to-head of the crate's two routing passes — plain `route()` (identity layout, greedy) vs. `route_lookahead()` (SABRE-style, scores candidate SWAPs against a lookahead window) — on four benchmark circuits, reporting the SWAP-count and depth improvement, with a correctness check that both passes are exactly semantics-preserving. Ends with a concrete, actionable finding: `backend::lower` currently calls the plain pass, not the smarter one already sitting in the codebase.

```bash
cargo run --example layout_comparison
```

### `backend_cost_comparison`

Builds a few standard circuits (Bell pair, GHZ, QFT) programmatically — no QASM — lowers each to every backend, and recommends the backend with the highest estimated fidelity for each circuit shape. The right starting point when you're choosing a target backend before writing real code against it.

```bash
cargo run --example backend_cost_comparison
```

---

## Fidelity & external benchmarking

### `fidelity_scaling`

Scales a GHZ-state circuit from 2 to 98 qubits (98 matches Quantinuum Helios's own qubit count) and tracks estimated fidelity as it drops. Makes a specific point directly: it's *gate count*, not *qubit count*, that drives the fidelity budget down, and a compiler that avoids unnecessary gates matters more as circuits grow.

```bash
cargo run --example fidelity_scaling
```

### `qiskit_benchmark`

The real external benchmark: this crate's routing and lowering measured against Qiskit's actual `transpile()`, on the same set of circuits (GHZ, hardware-efficient ansatz, layered random circuits, Bernstein-Vazirani, QAOA MaxCut, Trotterized Ising, QPE, and more), targeting the same real IBM basis gate set. Exports QASM and coupling maps for a companion `qiskit_transpile_compare.py` script to consume, so the comparison is apples-to-apples rather than self-reported.

**Requires:** Python 3 with Qiskit installed, to run the comparison half (`python3 qiskit_transpile_compare.py`) — the Rust side runs standalone.

```bash
cargo run --example qiskit_benchmark
python3 qiskit_transpile_compare.py
```

---

## Correctness

### `verify_equivalence`

The crate's actual correctness harness, not just a demo: 40 randomized circuits (2-5 qubits, 12-30 gates each, fixed seed for reproducibility), every rewrite the compiler performs — source-level `optimize_ir`, native `{Rz, Ry, Rzz}` decomposition, and all three backend lowerings — checked by running both the reference and the rewritten circuit on a real simulator and computing state fidelity between them. "The gate count looks right" is not proof a rewrite preserved semantics; this is the check that actually is.

**What you'll learn:** how to verify a compiler transformation is correct, not just plausible — the same technique the crate's own internal test suite relies on.

```bash
cargo run --example verify_equivalence
```

---

## Algorithms & applications

Three complete, real algorithms — not toy demonstrations — each running the full pipeline: ideal-simulator validation, real backend routing and fidelity comparison, real Monte-Carlo hardware noise, and (where applicable) statistically rigorous zero-noise extrapolation with propagated uncertainty, not just a point estimate.

### `vqe_h2_ground_state`

Variational Quantum Eigensolver finding the ground-state energy of molecular hydrogen. Verified against closed-form diagonalization (so "correct" is known exactly, not estimated), optimized against the ideal simulator, routed and lowered across every backend, then run with real simulated hardware noise and mitigated with zero-noise extrapolation — reporting whether the mitigated result clears the field-standard chemical-accuracy threshold, with an honest statistical significance check rather than a bare pass/fail.

```bash
cargo run --release --example vqe_h2_ground_state
cargo run --release --example vqe_h2_ground_state -- --noise-shots 50000
```

### `qaoa_portfolio_optimization`

QAOA solving a small Markowitz portfolio-selection problem — mean-variance optimization with a cardinality budget and a per-sector diversification cap, recast as a QUBO and then an Ising Hamiltonian. Checked against brute-force enumeration (8 assets = 256 states, exactly solvable), then run through the same real routing + noise + zero-noise-extrapolation pipeline as the VQE example. Explicit about what it isn't claiming: no quantum advantage at this problem size, and the asset data is synthetic, not investment advice.

```bash
cargo run --release --example qaoa_portfolio_optimization
cargo run --release --example qaoa_portfolio_optimization -- --p-layers 2 --shots 4096
```

### `trotter_ising_dynamics`

Trotterized time evolution of a transverse-field Ising spin chain — the same experiment shape as IBM's 2023 "utility" demonstration (Kim et al., *Nature* 618, 500). Verified against an independent 4th-order Runge-Kutta integration of the actual Schrödinger equation (no circuit or diagonalization involved), with a convergence sweep that doubles as a self-check on the compiler's gate-angle conventions. Includes an error decomposition separating algorithmic (Trotter step-count) error from hardware-noise error — since zero-noise extrapolation can only ever fix the latter — and a step-count trade-off sweep that finds the total-error-minimizing configuration empirically rather than by guessing.

```bash
cargo run --release --example trotter_ising_dynamics
cargo run --release --example trotter_ising_dynamics -- --trotter-steps 16 --noise-shots 150000
```

---

## Companion scripts

Two examples hand off to standalone Python scripts that live at the repository root, for steps this crate deliberately doesn't do itself (talking to real IBM Quantum hardware, and calling Qiskit's transpiler for comparison). Both are optional — the Rust examples above run and produce complete output without them.

### `submit_ibm.py`

The bridge from this crate's real IBM-basis QASM output to actual execution — there's no official Rust SDK for IBM Quantum Platform / Qiskit Runtime, so this script is the intended handoff point. It takes QASM from `ibm_export::to_ibm_qasm` (what [`bell_state_end_to_end`](#bell_state_end_to_end) and [`pipeline_end_to_end`](#pipeline_end_to_end) produce) and does one of three things: run it on a local Qiskit Aer simulator (no account needed), submit it to real IBM Quantum hardware, or query a real backend's live coupling map, basis gates, and calibration data.

**Local sanity check** (confirms the exported QASM parses and runs — validates the export plumbing, not real-device behavior):

```bash
python3 submit_ibm.py --qasm bell.qasm --shots 4096
```

**Real hardware:**

```bash
export IBM_QUANTUM_TOKEN=...       # from your quantum.ibm.com account settings
export IBM_QUANTUM_INSTANCE=...    # CRN of your instance/plan
python3 submit_ibm.py --qasm bell.qasm --shots 4096 --backend <ibm_backend_name> --real
```

Submits with `optimization_level=0` deliberately — routing and native-gate lowering already happened on the Rust side, so letting Qiskit re-transpile here would mean testing Qiskit's transpiler output instead of this crate's.

**Compare against a reference distribution** (e.g. the one [`bell_state_end_to_end`](#bell_state_end_to_end) writes) with `--compare`, which reports total variation distance — 0.0 is identical, 1.0 is fully disjoint; real hardware is never expected to hit 0.0 exactly, so this is "how close," not "did it match exactly":

```bash
python3 submit_ibm.py --qasm bell.qasm --real --backend <name> \
  --compare bell_reference_counts.json
```

**Dump a real backend's live topology and calibration** — read-only, doesn't spend shots — as JSON your Rust code can consume directly: `edges`/`num_qubits` feed `CouplingMap::from_edges` (routing against the device's *actual*, possibly-irregular coupling graph instead of the crate's synthetic `heavy_hex_for` topology), `basis_gates` feeds `ibm_export::validate_cx_native_basis` (catches an ECR-native device before QASM export silently produces gates it can't run), and `single_qubit_fidelity`/`two_qubit_fidelity` build a fresh `PublishedCalibration` from today's live numbers instead of `fidelity.rs`'s fixed published snapshot:

```bash
export IBM_QUANTUM_TOKEN=...
export IBM_QUANTUM_INSTANCE=...
python3 submit_ibm.py --dump-coupling-map --backend <ibm_backend_name> --out coupling.json
```

**Requires:** `pip install qiskit qiskit-ibm-runtime` for `--real` or `--dump-coupling-map`; plain `qiskit` is enough for the local-simulator path, which is the default.

### `qiskit_transpile_compare.py`

The other half of [`qiskit_benchmark`](#qiskit_benchmark)'s comparison. `qiskit_benchmark` writes each benchmark circuit's QASM to `./qiskit_benchmark_qasm/*.qasm`, plus the exact `CouplingMap::heavy_hex_for(n)` edge list it routed against to `*_coupling.txt`. This script reads both, runs each circuit through Qiskit's own `transpile()` — targeting the same real IBM native basis (`{rz, sx, x, cx}`) *and* the same heavy-hex coupling map, at `optimization_level=3` — and prints a table (source gate count, output depth, total basis-gate count, two-qubit gate count) directly comparable to `qiskit_benchmark`'s own output columns for the same benchmark name.

The coupling-map matching is the important detail: without it, Qiskit would be transpiling against unconstrained all-to-all connectivity — a strictly easier problem than the real sparse hardware topology this crate routes against — and a gate-count gap under that mismatch would just be measuring "no routing needed" vs. "real routing needed," not transpiler quality. With matching constraints, both sides solve the identical problem.

```bash
cargo run --example qiskit_benchmark        # writes the QASM + coupling files first
python3 qiskit_transpile_compare.py
```

**Requires:** `pip install qiskit`.

If you've also run [`layout_comparison`](#layout_comparison), its result explains *why* this crate's numbers currently lag Qiskit's on nearest-neighbor-structured circuits even under an identical coupling map: `backend::lower` calls the plain `route()` pass (identity layout, no lookahead) rather than the already-implemented, already-tested `route_lookahead()` (SABRE-style) pass — a concrete, measured gap, not a vague one.

---

## A note on statistical rigor

The three algorithm examples above (and `qiskit_benchmark`'s comparison methodology) share a design principle worth calling out explicitly: every noise-mitigation claim is checked against a propagated uncertainty, not reported as a bare number. Zero-noise extrapolation is a weighted least-squares fit (points weighted by measured precision), returning both an estimate and its own standard error, and every "mitigation improved things" claim is tested against that error before being reported as real. If you're building on top of these examples for your own results, that pattern — report the uncertainty, check significance before claiming improvement — is the one worth keeping.
