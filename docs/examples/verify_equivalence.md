# verify_equivalence

The crate's actual correctness harness — not a demo dressed up as one. Proves, rather than asserts, that every rewrite the compiler performs preserves a circuit's real behavior.

```bash
cargo run --example verify_equivalence
```

## The core idea

"The gate count looks reasonable" is not proof a rewrite preserved semantics. The only check that actually means something: run both the reference circuit and the rewritten one against a real `QuantumRegister` starting from `|00...0⟩`, then compute state fidelity between the two final states. Fidelity is 1.0 if and only if the two states are identical up to global phase — exactly the invariant every rewrite in `native.rs`/`backend.rs` claims to preserve.

```rust
let reference = run_native(circuit);
let after_optimize_ir = run_native(&optimize_ir(circuit));
let fid_optimize_ir = reference.fidelity(&after_optimize_ir).expect("fidelity");
```

This is the same check `backend.rs`'s own internal `check_backend_matches` test uses — this example just runs it over many more circuits and reports the results as a table, plus extends the check to `optimize_ir` (the source-level rewrite), which the existing test suite didn't cover end-to-end against the simulator before this example existed.

## What it does

Generates 40 randomized circuits (2-5 qubits, 12-30 gates each, drawn from the full source-level `Gate` set except `Measure`, fixed seed for a reproducible report) and, for each, checks:

- **`optimize_ir`** — does source-level cancellation/reordering change the circuit's action at all?
- **Each backend's lowering** (`TrappedIon`, `IbmQ`, `Rigetti`) — native decomposition, plus routing where the backend has a coupling map — against the same reference.

```rust
for &backend in &BACKENDS {
    let bc = lower(circuit, backend);
    let reg = emit::run_backend(&bc).expect("backend run");
    let fid = reference.fidelity(&reg).expect("fidelity");
}
```

Every circuit is unitary-only (no `Measure`) — a projective measurement collapses state non-deterministically, so "run twice, expect agreement" isn't a meaningful comparison post-measurement. Backends that route (`IbmQ`, `Rigetti`) still get the full test: `route::route`'s final restoration pass is supposed to put every logical qubit back on its original wire, so fidelity against the unrouted reference should still land at 1.0 if that guarantee holds.

## Output

A table of per-circuit fidelities (one row per random circuit, one column per rewrite/backend), a worst-fidelity-observed summary line, and a final `PASS`/`FAIL` verdict — `FAIL` exits with a non-zero status code, so this is suitable for wiring into CI, not just a printed report.

## Why it matters

This is the example to point to when someone asks "how do you know the compiler's optimizations don't silently change behavior?" It's also the right template to copy if you're adding a new rewrite pass and want the same category of proof for it, not just a hand-picked example that happens to work.

## Related

- [`routing_demo`](routing_demo.md), [`layout_comparison`](layout_comparison.md) — both use this same fidelity-based check on their specific circuits, at smaller scale
