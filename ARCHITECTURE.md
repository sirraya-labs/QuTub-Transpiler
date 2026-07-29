# Architecture — sirraya-qutub-transpiler

This is the deep-dive reference: what each module does, why it's built the
way it is, and what's still open. **If you're looking for how to set up
the project, run tests, or open a PR, see [`CONTRIBUTING.md`](CONTRIBUTING.md)
instead** — that's the short version. Come back here when you're about to
touch a specific module, or want the full reasoning behind a design choice
your PR review references.

## 1. What this crate is

A QASM 2.0 importer and multi-backend native-gate compiler for circuits
that ultimately run on `sirraya_qutub::core::QuantumRegister` — Sirraya
Labs' own statevector simulator. You hand it OPENQASM 2.0 text (or build a
`Circuit` directly), and it:

1. Parses it into a rich source-level gate set,
2. Optimizes at that source level,
3. Decomposes into a specific target's native gate set (routing physical
   qubits first, if the target needs it),
4. Optimizes again at the native level,
5. Gives you a quick fidelity estimate,
6. Actually runs it against the real simulator (or emits it back out as
   QASM).

Every one of those steps is its own module, and — this is the crate's most
important convention — **every non-trivial identity in every module is
checked against the real simulator, not just asserted algebraically.**
More on that in §4.

## 2. The pipeline, end to end

```
qasm::parse                (text -> ir::Circuit)
      |
ir_optimize::optimize      (source-level cancel/reorder)
      |
route::route               (insert Swaps against a CouplingMap, if the
      |                      target backend needs one)
      |
      +-- native::decompose        --> optimize::optimize        (TrappedIon path)
      |
      +-- backend::lower           --> backend::optimize/resynthesize (IbmQ/Rigetti path)
      |
fidelity::estimate_*_fidelity      (quick sanity-check number, no simulation)
      |
emit::run / emit::run_backend      (actually execute on sirraya_qutub)
emit::to_qasm                      (or emit back out as QASM text)
```

`lib.rs`'s own doc comment has this same diagram — it's the map to keep
open in a second tab.

## 3. Module-by-module

### `ir.rs` — the source gate set
`Gate` is deliberately rich: it mirrors `QuantumRegister`'s whole `apply_*`
surface (`H`, `X`/`Y`/`Z`, `S`/`Sdg`, `T`/`Tdg`, `Rx`/`Ry`/`Rz`, `Cx`, `Cz`,
`Swap`, `Rxx`/`Ryy`/`Rzz`, `Cp`, and `Measure`). The narrowing to a
hardware-native set doesn't happen here — that's `native.rs`'s and
`backend.rs`'s job. `Circuit` also carries `num_clbits`, mirroring how
`num_qubits` works, since `Gate::Measure` needs somewhere to write.

`Gate::Measure` is the one variant that isn't a unitary rewrite target —
it's a classical side effect, and it shows up as a special case in several
other modules (see below) precisely because it can't be treated like every
other gate.

### `qasm.rs` — the importer
A **subset** OPENQASM 2.0 parser: exactly the dialect
`sirraya_qutub::QuantumCircuit::to_qasm`/`QuantumRegister::to_qasm` write,
plus the common `qelib1.inc` mnemonics other tools (Qiskit, etc.) use for
the same gate set. No gate definitions, no classical control, no includes
beyond one `qreg`/`creg` pair, no barriers — anything outside that subset
is a parse error naming the offending line, not a silent skip. `measure
q[i] -> c[j];` is range-checked against the declared `qreg`/`creg` sizes at
parse time.

### `ir_optimize.rs` — source-level optimization
Two things: literal self-inverse/explicit-inverse cancellation (`H;H`,
`S;Sdg`, `Cx(a,b);Cx(a,b)`, zero-angle/angle-negating rotation pairs, …),
and a commuting-reorder pass that slides gates past each other so
non-adjacent cancellable pairs become adjacent. The **only** commutation
rule used is the universally true one — disjoint qubit sets commute
unconditionally — deliberately, not gate-specific rules like "Rz commutes
through a CNOT control" (that's real, and `backend.rs`'s peephole pass does
use rules like it, but each one needs its own derivation and test; this
module stays conservative).

`Gate::Measure` is special-cased to **never** be treated as commutable
past anything, in either direction — even past a gate that's qubit-disjoint
from it. Two `Measure`s writing different qubits into the *same* classical
bit are only disjoint by qubit, not by the classical side effect that
matters, so this module can't yet reason about classical-bit dependencies
precisely enough to reorder them safely.

### `native.rs` — the trapped-ion-style native gate set
Decomposes anything in `ir::Gate` into `{Rz, Ry, Rzz}` — the gate set
`sirraya_qutub`'s own Quantinuum Helios `HardwareCalibration` story is
actually about. Every identity here is exact, not approximate, including:
- ZYZ Euler decomposition (`zyz_decompose`) for arbitrary single-qubit
  unitaries, via a small private complex/2×2-matrix algebra
  (`C`/`Mat2`/`matmul`) kept local to this module so the synthesizer
  doesn't need to reach into `sirraya_qutub`'s own complex representation.
- `Cx = H(target) . Cp(control,target,pi) . H(target)`.
- `Swap = Cx(a,b);Cx(b,a);Cx(a,b)`.
- `Rxx(theta) = (H⊗H) . Rzz(theta) . (H⊗H)` (since `X = H.Z.H`).
- `Ryy(theta) = (Rx(pi/2)⊗Rx(pi/2)) . Rzz(theta) . (Rx(-pi/2)⊗Rx(-pi/2))`
  (since `Y = Rx(-pi/2).Z.Rx(-pi/2)^dagger`).

`C`/`Mat2` and the matrix builders are `pub(crate)` rather than fully
private specifically so `backend.rs`'s `resynthesize` pass can reuse the
*same*, already-validated ZYZ algebra instead of re-deriving a second copy.

### `backend.rs` — multi-backend lowering
Lowers a `Circuit` to one of three `Backend`s' actual native gate sets,
each modeled on a real device family:
- `TrappedIon` — `{Rz, Ry, Rzz}`. Delegates straight to `native::decompose`.
- `IbmQ` — `{Rz, Rx, Cx}` (virtual-Z + native CNOT).
- `Rigetti` — `{Rz, Rx, Cz}` (CZ-native, no native CNOT).

Two new exact identities do the work on top of `native.rs`'s: `Ry(theta) =
Rx(-pi/2).Rz(theta).Rx(pi/2)`, and `Rzz(a,b,theta) =
Cx(a,b).Rz(b,theta).Cx(a,b)`. Rigetti doesn't naively substitute `Cx` via
`Cz` twice (4 `H`'s) — it uses a third identity, `H.Rz(theta).H =
Rx(theta)`, to collapse the middle of that expansion and land on 2 `H`'s
instead.

Two cleanup passes run in this module, to a fixed point:
- `optimize` — a peephole pass: adjacent same-axis `Rot` fusion, `Rz`
  commutation through diagonal-compatible gates (`Cz`/`Rzz` on either
  wire, `Cx`'s *control* wire only — **not** its target wire, that's the
  trap the tests specifically pin down), and same-pair `Cx`/`Cz`/`Rzz`
  cancellation/fusion.
- `resynthesize` — a strictly stronger version of the above: accumulates
  the *whole* product matrix of a maximal single-qubit run (even across
  real intervening `Rz`s that `optimize` correctly refuses to merge
  across) and re-synthesizes it from scratch via `zyz_decompose`, so a
  6+-gate run collapses to at most 3 gates regardless of how it got that
  long. `lower` runs `resynthesize`/`optimize` back-to-back to a fixed
  point, since a cancellation freed up by one can expose more work for the
  other.

`Backend::coupling_map` is what ties this module to `coupling.rs`/
`route.rs`: `None` for `TrappedIon` (a trapped-ion chain's shared motional
mode makes every pair directly reachable), `heavy_hex_for` for `IbmQ`
(P1.1), `square_grid_for` for `Rigetti` (P1.3) — see below.

**Deliberately not here:** `Backend::Pasqal`. Neutral-atom platforms need
atom *placement* and blockade-radius routing, not just "express this
unitary in a fixed two-qubit gate" — modeling it as a `Rigetti`-alike would
misrepresent an untested backend as equivalent to the two that were
actually tested the same way `native.rs` was. Left as real future work,
not a shortcut.

### `coupling.rs` — physical qubit connectivity
`CouplingMap` models which physical qubits a backend's native two-qubit
gate can be applied to directly.
- `linear(n)` — a chain, `q` adjacent only to `q+1`. No longer used by any
  `Backend` (see below), but kept as the topology-free stand-in for the
  0/1-qubit case both real-topology constructors fall back on.
- `heavy_hex_grid(rows, cols)` / `heavy_hex_for(n)` — **(P1.1)** the real
  heavy-hex lattice family IBM's superconducting processors (Eagle, Heron,
  …) actually use: a hexagonal lattice of degree-≤3 "data" qubits with a
  degree-2 "flag" qubit subdividing every edge. `heavy_hex_for(n)` finds
  the smallest such grid with ≥ `n` qubits and takes a BFS-order prefix of
  exactly `n` — guaranteed connected, since a BFS prefix of a connected
  graph always is. This is the actual topology family, not a claim about
  any specific chip's exact qubit numbering.
- `square_grid(rows, cols)` / `square_grid_for(n)` — **(P1.3)** the real
  square-lattice family Rigetti's current Ankaa-class processors (Ankaa-2,
  Ankaa-3) actually use: a plain rectangular grid, interior qubits with
  four-fold connectivity (edges 3, corners 2) — *not* the square-octagonal
  unit cell of Rigetti's earlier Aspen generation. `square_grid_for(n)`
  finds the smallest square grid with ≥ `n` qubits and takes the same kind
  of BFS-order prefix `heavy_hex_for` does, for the same connectivity
  guarantee.
- `neighbors(q)` — **(P1.2)** adjacency-list access, added so `route.rs`
  can build a spanning tree of the graph. (`is_adjacent` only answers a
  yes/no for one pair; `shortest_path` is point-to-point; neither gives you
  the graph structure itself.)
- `shortest_path(a, b)` — plain BFS, `None` if disconnected.

### `route.rs` — SWAP insertion
Inserts `Swap`s into a *source-level* circuit (before `native::decompose`
or `backend::lower` ever see it) so every two-qubit gate lands on
coupling-adjacent physical qubits. Tracks a `logical -> physical` mapping,
re-addresses single-qubit gates to wherever their logical qubit currently
sits, and for a non-adjacent two-qubit gate, walks the *first* argument's
qubit toward the second's (fixed) location along the BFS shortest path,
inserting one `Swap` per hop. Argument order is preserved throughout —
`Cx(control, target)` is asymmetric, so routing always moves the first
argument, never picks arbitrarily, and never risks silently swapping
control and target.

Once every gate is processed, `restore_identity_mapping` sorts every qubit
back onto its original physical wire — **not optional bookkeeping**: each
`Swap` used mid-routing displaces some *other*, possibly never-gated qubit
sideways too, and this crate has no way yet to translate a final permuted
wire arrangement back to logical order, so leaving it in place would be a
silent correctness bug (fidelity against a reference simulator would
legitimately differ even though no gate's logic was ever wrong).

**(P1.2)** `restore_identity_mapping` used to be a plain adjacent-index
bubble sort, which silently assumed physical qubits are numbered along a
path where consecutive indices are always coupling-adjacent — true for
`CouplingMap::linear`, **false** for `heavy_hex_for`'s degree-3 topology.
It's now a general, connectivity-correct token-swap pass: build a BFS
spanning tree of the coupling graph, then repeatedly prune a leaf — if it
already holds its own token, retire it; otherwise walk its home token to it
one tree-adjacent swap at a time. This is a *correctness* pass, not a
swap-count-optimal one (optimal token swapping is NP-hard on general
graphs), matching this module's stated non-goal of SWAP-count
minimization generally.

`Gate::Measure` is single-qubit-shaped for routing purposes — remapped in
place at the point it's encountered, tracking whatever physical wire its
qubit is *currently* on, not waiting for the final restore pass.

### `optimize.rs` — native-level peephole pass
A smaller sibling of `backend.rs`'s peephole pass, over `NativeCircuit`
(`{Rz, Ry, Rzz}`) instead of a `BackendCircuit`. Two passes to a fixed
point: merge adjacent same-axis `Rz`/`Ry`/`Rzz` on the same qubit(s), and
drop any gate whose combined angle is exactly zero. `Measure` is never a
candidate for either — it's not a rotation with an angle to net to zero,
and it's a real classical side effect the caller depends on.

### `emit.rs` — actually running things
The one module that actually touches `sirraya_qutub`. `run`/`apply_to`
execute a `NativeCircuit` (or `run_backend`/`apply_backend_to` for a
`BackendCircuit`) against a real `QuantumRegister`, and error on `Measure`
since that entry point has nowhere to put a classical outcome.
`run_with_measurement`/`apply_to_with_measurement` (and the `_backend`
equivalents) are the real answer for a circuit that measures — `Measure`
is executed for real via `QuantumRegister::measure_single_qubit`, a
confirmed genuine Born-rule-sampled projective measurement that collapses
and renormalizes the state vector. `to_qasm` round-trips a `NativeCircuit`
back out as `sirraya_qutub`-dialect QASM, including a `creg` sized to
`num_clbits` (not hardcoded to `num_qubits`, which would silently break
round-tripping for any circuit whose clbit count differs).

### `fidelity.rs` — quick fidelity budgeting
A fast, gate-count-based fidelity estimate (independent-depolarizing-event
approximation) — not a substitute for actually running XEB, just a
sanity-check before paying for a full noisy simulation.
`PublishedCalibration` used to independently re-derive
`sirraya_qutub::xeb::HardwareCalibration`'s formula and Quantinuum Helios
numbers out of caution; **(P0.2)** that divergence risk is now confirmed
never to have existed (the two types are field-for-field identical), so
`quantinuum_helios_2026()` is now a thin `From`-based wrapper delegating
to the real type, and a test pins the two numbers together so they can't
silently drift apart in the future. `ibm_heron_r2()`/`rigetti_ankaa3()`
have no counterpart in `sirraya_qutub::xeb` and still stand on their own
cited sources — read their doc comments before treating either figure as
more authoritative than it is (in particular, `rigetti_ankaa3`'s
single-qubit figure is carried over from the *previous* device
generation, flagged explicitly as such).

## 4. The testing philosophy (read this before writing a new identity)

**Every gate identity, decomposition, or optimization pass is checked
against the real `sirraya_qutub::core::QuantumRegister`, not just asserted
algebraically.** The recurring pattern, used in `tests/decompositions.rs`,
`ir_optimize.rs`'s in-module tests, `route.rs`'s in-module tests, and
`backend.rs`'s in-module tests:

1. Build a randomized initial state (a random product of single-qubit
   rotations, so every amplitude is nonzero and phase-sensitive).
2. Run the "ground truth" side directly on a cloned `QuantumRegister`
   using its own `apply_*` methods.
3. Run the transpiler's side (decompose/route/optimize/lower, then
   execute) on another clone of the same initial state.
4. Compare via `QuantumRegister::fidelity`, asserting `(fidelity - 1.0).abs()
   < 1e-9`.

A wrong sign anywhere in the algebra reliably shows up as fidelity ≪ 1, not
a subtle discrepancy — these identities are either exactly right or
clearly wrong, which is why this method is trusted over static review.

**`Gate::Measure` can't use this methodology** — `fidelity` doesn't apply
to a collapsed state. `tests/measurement.rs` instead runs many shots (4000)
through the real `measure_single_qubit` path and compares the empirical
outcome frequency against the ideal Born-rule probability (computed by
running the *same* circuit with its `Measure`s simply not executed, read
via `get_measurement_probability`), within a wide statistical tolerance (6
standard errors) chosen so real bugs fail reliably while sampling noise
essentially never does.

If you add a new identity anywhere in this crate, add a fidelity- (or, for
`Measure`, shot-) based test in this same style. An algebraically-derived
identity that only has an `assert_eq!` on a hand-computed matrix is not
held to this crate's standard.

## 5. Current status

**Done:**
- **P0.1** — `Gate::Measure` end to end: parsing (`qasm.rs`), the source
  and native IR (`ir.rs`/`native.rs`), routing (`route.rs`), backend
  lowering (`backend.rs`), real execution with classical outcomes
  (`emit.rs`), and the shot-based statistical test methodology
  (`tests/measurement.rs`).
- **P0.2** — Confirmed `fidelity::PublishedCalibration` and
  `sirraya_qutub::xeb::HardwareCalibration` are field-for-field identical
  for their one shared entry (Quantinuum Helios); replaced the
  independent re-derivation with a thin, test-pinned wrapper so the two
  numbers can't silently diverge.
- **P1.1** — `IbmQ` now routes against a real heavy-hex lattice
  (`CouplingMap::heavy_hex_for`/`heavy_hex_grid`), not a linear-chain
  stand-in.
- **P1.2** — `route.rs`'s final identity-restoration pass is now
  general-graph-correct (spanning-tree token swapping), not a bubble sort
  that silently assumed consecutive physical indices are always
  coupling-adjacent — a real, live bug against `IbmQ`'s heavy-hex map, not
  a theoretical one.

**Open / known gaps, in the modules' own words:**
- `Rigetti` is still modeled with a conservative `linear` coupling map,
  not its real (more permissive) grid topology — safe today because
  routing that succeeds against a line also succeeds against the grid, but
  it's not yet *using* Rigetti's actual connectivity, only bounded by it.
- `Backend::Pasqal` (neutral atoms) isn't implemented — it needs atom
  placement and blockade-radius routing, a materially different problem
  from fixed native-gate lowering, and a "digital-mode" `Rigetti`-alike
  stand-in was explicitly rejected as misrepresenting an untested backend.
- No SWAP-count minimization anywhere in `route.rs` (no reordering
  independent gates, no lookahead for a later gate) — it's a correctness
  pass by design, not a routing optimizer.
- `ir_optimize.rs`'s commuting pass only ever uses disjoint-qubit-set
  commutativity — no gate-specific commutation rules (e.g. "Rz commutes
  through a CNOT control", which `backend.rs`'s peephole pass *does* use)
  are implemented at the source level yet.
- `route.rs`'s new token-swap restoration is correctness-focused, not
  swap-count-optimal (optimal token swapping is NP-hard on general
  graphs) — fine for this crate's goals, but worth knowing if a future
  contributor is tempted to "just minimize the swaps."

## 6. See also

[`CONTRIBUTING.md`](CONTRIBUTING.md) has the setup instructions, test
commands, coding conventions, and PR checklist — this document is
reference material for *what's here and why*, not a how-to.