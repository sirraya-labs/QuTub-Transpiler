# Examples

The repository ships fourteen runnable examples in `examples/`, each with its own dedicated page below. Every one is real, working code against the actual compiler — nothing here is pseudocode or a notebook stand-in.

Run any example with:

```bash
cargo run --release --example <name>
```

(Use `--release` for anything that runs an optimization loop or a Monte-Carlo noise sweep — several examples below are 10-50x slower in debug builds.)

## I want to...

| ...do this | Start with |
|---|---|
| See the whole compiler work end to end for the first time | [`bell_state_end_to_end`](examples/bell_state_end_to_end.md) |
| Understand what a specific gate actually costs on real hardware | [`gate_cheatsheet`](examples/gate_cheatsheet.md) |
| See a circuit change shape as it moves through compilation | [`diagram_demo`](examples/diagram_demo.md) |
| Pick the best backend for a circuit I'm building | [`backend_cost_comparison`](examples/backend_cost_comparison.md) |
| Understand why connectivity, not gate translation, is what really costs fidelity | [`routing_demo`](examples/routing_demo.md) |
| Know whether the compiler's routing is actually competitive | [`layout_comparison`](examples/layout_comparison.md) or [`qiskit_benchmark`](examples/qiskit_benchmark.md) |
| Convince myself a compiler rewrite didn't silently change my circuit | [`verify_equivalence`](examples/verify_equivalence.md) |
| See a real variational algorithm, with real hardware noise and real error mitigation | [`vqe_h2_ground_state`](examples/vqe_h2_ground_state.md) or [`qaoa_portfolio_optimization`](examples/qaoa_portfolio_optimization.md) |
| See a physics simulation benchmarked against an independent classical reference | [`trotter_ising_dynamics`](examples/trotter_ising_dynamics.md) |

---

## Start here

Three short examples that between them touch every stage of the compiler at least once. Read these before anything else.

### [`bell_state_end_to_end`](examples/bell_state_end_to_end.md)

The smallest complete pipeline run: parse → optimize → lower → export/execute, in its smallest possible form. Produces real files (`bell.qasm`, `bell_reference_counts.json`) meant to be handed to `submit_ibm.py` for a real-hardware comparison.

### [`gate_cheatsheet`](examples/gate_cheatsheet.md)

For every source-level gate, the exact native `{Rz, Ry, Rzz}` sequence it costs, before and after optimization — what does this gate actually cost, at a glance.

### [`diagram_demo`](examples/diagram_demo.md)

Renders the same circuit at three compilation stages (source, native, backend-lowered) as ASCII art, plus an SVG export.

---

## Compiler pipeline, end to end

### [`full_pipeline`](examples/full_pipeline.md)

QASM in, source-level optimization, native decomposition + fidelity estimate, then the same circuit lowered and *executed* on all three backends, each judged against its own published calibration.

### [`pipeline_end_to_end`](examples/pipeline_end_to_end.md)

The fuller version: adds diagrams at every stage, real Born-rule-sampled measurement outcomes, and a real IBM-basis QASM export ready for Qiskit Runtime. The single file to read if you want the entire compiler surface in one place.

---

## Hardware, routing & topology

### [`routing_demo`](examples/routing_demo.md)

Makes SWAP-insertion cost visible: routes a connectivity-hostile circuit against IBM heavy-hex, Rigetti square-grid, and a worst-case linear chain, with a correctness proof that routing never changes the circuit's actual output.

### [`layout_comparison`](examples/layout_comparison.md)

Head-to-head of the crate's two routing passes — plain `route()` vs. the smarter, already-implemented `route_lookahead()` — quantifying the SWAP-count win and explaining exactly why `qiskit_benchmark`'s numbers currently lag Qiskit's.

### [`backend_cost_comparison`](examples/backend_cost_comparison.md)

Builds standard circuits (Bell pair, GHZ, QFT) programmatically and recommends the best backend per circuit by estimated fidelity — the right starting point before committing to a target.

---

## Fidelity & external benchmarking

### [`fidelity_scaling`](examples/fidelity_scaling.md)

Scales a GHZ circuit from 2 to 98 qubits (98 = Quantinuum Helios's own qubit count), showing that gate count, not qubit count, is what actually drives fidelity down.

### [`qiskit_benchmark`](examples/qiskit_benchmark.md)

The real external benchmark: this crate's routing and lowering measured against Qiskit's actual `transpile()`, on 14 circuits chosen to stress-test different router behaviors, targeting the same real IBM basis and coupling map. Includes a restoration-tax breakdown of exactly how much SWAP overhead is mandatory vs. bookkeeping.

**Requires:** Python 3 with Qiskit installed for the comparison half.

---

## Correctness

### [`verify_equivalence`](examples/verify_equivalence.md)

The crate's actual correctness harness: 40 randomized circuits, every rewrite the compiler performs checked by running both the reference and rewritten circuit on a real simulator and computing state fidelity — proof, not assertion.

---

## Algorithms & applications

Three complete, real algorithms — each running ideal-simulator validation, real backend routing, real Monte-Carlo hardware noise, and statistically rigorous zero-noise extrapolation with propagated uncertainty, not a bare point estimate.

### [`vqe_h2_ground_state`](examples/vqe_h2_ground_state.md)

Variational Quantum Eigensolver finding H₂'s ground-state energy, verified against closed-form diagonalization, then mitigated against real simulated hardware noise — reporting whether the result clears the field-standard chemical-accuracy threshold, with an honest statistical significance check.

### [`qaoa_portfolio_optimization`](examples/qaoa_portfolio_optimization.md)

QAOA solving a small Markowitz portfolio-selection problem, checked against brute-force enumeration, then run through the same real routing + noise + ZNE pipeline as the VQE example.

### [`trotter_ising_dynamics`](examples/trotter_ising_dynamics.md)

Trotterized time evolution of a transverse-field Ising chain — the same experiment shape as IBM's 2023 "utility" paper — verified against an independent RK4 integration of the Schrödinger equation, with an error decomposition separating algorithmic error from hardware-noise error.

---

## Companion scripts

Two examples hand off to standalone Python scripts that live at the repository root, for steps this crate deliberately doesn't do itself (talking to real IBM Quantum hardware, and calling Qiskit's transpiler for comparison). Both are optional — the Rust examples above run and produce complete output without them.

### `submit_ibm.py`

The bridge from this crate's real IBM-basis QASM output to actual execution — there's no official Rust SDK for IBM Quantum Platform / Qiskit Runtime, so this script is the intended handoff point. It takes QASM from `ibm_export::to_ibm_qasm` (what [`bell_state_end_to_end`](examples/bell_state_end_to_end.md) and [`pipeline_end_to_end`](examples/pipeline_end_to_end.md) produce) and does one of three things: run it on a local Qiskit Aer simulator (no account needed), submit it to real IBM Quantum hardware, or query a real backend's live coupling map, basis gates, and calibration data.

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

**Compare against a reference distribution** (e.g. the one [`bell_state_end_to_end`](examples/bell_state_end_to_end.md) writes) with `--compare`, which reports total variation distance — 0.0 is identical, 1.0 is fully disjoint; real hardware is never expected to hit 0.0 exactly, so this is "how close," not "did it match exactly":

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

The other half of [`qiskit_benchmark`](examples/qiskit_benchmark.md)'s comparison. `qiskit_benchmark` writes each benchmark circuit's QASM to `./qiskit_benchmark_qasm/*.qasm`, plus the exact `CouplingMap::heavy_hex_for(n)` edge list it routed against to `*_coupling.txt`. This script reads both, runs each circuit through Qiskit's own `transpile()` — targeting the same real IBM native basis (`{rz, sx, x, cx}`) *and* the same heavy-hex coupling map, at `optimization_level=3` — and prints a table (source gate count, output depth, total basis-gate count, two-qubit gate count) directly comparable to `qiskit_benchmark`'s own output columns for the same benchmark name.

The coupling-map matching is the important detail: without it, Qiskit would be transpiling against unconstrained all-to-all connectivity — a strictly easier problem than the real sparse hardware topology this crate routes against — and a gate-count gap under that mismatch would just be measuring "no routing needed" vs. "real routing needed," not transpiler quality. With matching constraints, both sides solve the identical problem.

```bash
cargo run --example qiskit_benchmark        # writes the QASM + coupling files first
python3 qiskit_transpile_compare.py
```

**Requires:** `pip install qiskit`.

If you've also read [`layout_comparison`](examples/layout_comparison.md), its result explains *why* this crate's numbers currently lag Qiskit's on nearest-neighbor-structured circuits even under an identical coupling map: `backend::lower` calls the plain `route()` pass rather than the already-implemented, already-tested `route_lookahead()` pass — a concrete, measured gap, not a vague one.

---

## A note on statistical rigor

The three algorithm examples above (and `qiskit_benchmark`'s comparison methodology) share a design principle worth calling out explicitly: every noise-mitigation claim is checked against a propagated uncertainty, not reported as a bare number. Zero-noise extrapolation is a weighted least-squares fit (points weighted by measured precision), returning both an estimate and its own standard error, and every "mitigation improved things" claim is tested against that error before being reported as real. If you're building on top of these examples for your own results, that pattern — report the uncertainty, check significance before claiming improvement — is the one worth keeping.
