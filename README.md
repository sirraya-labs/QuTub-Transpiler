# sirraya-qutub-transpiler

A QASM 2.0 importer and native-gate compiler for circuits destined to
run on `sirraya-qutub`'s `QuantumRegister`. Decouples circuit
*description* from the specific gate set `sirraya-qutub` was calibrated
against, so a circuit written in whatever gates are convenient (H, CX,
T, RXX, ...) gets compiled down to the two kinds of operation
`HardwareCalibration` actually has a fidelity number for.

Depends on [`sirraya-qutub`](https://crates.io/crates/sirraya-qutub)
straight from crates.io (`0.1.13`) -- it's already published as a
library there, so no prerequisite changes to that repo are needed.

## Pipeline

```
QASM text --parse--> ir::Circuit --decompose--> native::NativeCircuit
    (rich gate set)          (trapped-ion-style {Rz, Ry, Rzz})
                                        |
                                    optimize (peephole cleanup)
                                        |
                    +-------------------+-------------------+
                    |                                       |
            fidelity::estimate_circuit_fidelity     emit::run (actually
            (fast, gate-count-based sanity check)     executes on
                                                        QuantumRegister)
```

- **`qasm`** -- a deliberately narrow OPENQASM 2.0 subset parser: the
  dialect `sirraya-qutub`'s own `to_qasm()` writes (`h q[0];`,
  `rzz(1.2) q[0], q[2];`, ...), plus the standard mnemonics other tools
  export for the same gate set. No gate definitions, no classical
  control -- anything outside that is a parse error naming the line, not
  a silent skip.
- **`native`** -- decomposes every gate down to `{Rz, Ry, Rzz}`: an
  arbitrary single-qubit rotation via ZYZ Euler decomposition, and every
  two-qubit gate via exact identities built on `Rzz` (CNOT via CZ via
  Rzz; RXX/RYY via basis-changed Rzz). This is the gate set
  `HardwareCalibration`'s single-qubit/two-qubit fidelity numbers are
  actually about.
- **`optimize`** -- peephole pass: merges adjacent same-axis rotations
  on the same qubit(s), drops rotations that cancel to ~0.
- **`fidelity`** -- a **self-contained** fidelity estimate. It does not
  import `sirraya_qutub::xeb::HardwareCalibration` and read its fields;
  it re-implements the one formula that module's doc comments publish
  (`p = (1-F)*d/(d-1)`) against the same published Quantinuum Helios
  numbers, independently of `sirraya-qutub`'s internal representation.
  See `src/fidelity.rs`'s doc comment for the reasoning -- this was a
  deliberate decoupling choice, not a limitation of what's actually
  accessible (the published crate's `HardwareCalibration` fields are
  public).
- **`emit`** -- the one module that actually touches the dependency:
  runs a `NativeCircuit` on a real `QuantumRegister`, and writes native
  circuits back out as `sirraya-qutub`-dialect QASM.

## Testing

Every decomposition identity is checked against
`sirraya_qutub::core::QuantumRegister` directly -- build a randomized
input state, apply the gate two ways (directly, vs. decomposed +
optimized native gates), compare via `QuantumRegister::fidelity`
(`tests/decompositions.rs`). A wrong sign shows up as fidelity << 1, not
a subtle discrepancy; the first version of the ZYZ synthesizer here
*did* fail this way (branch-ambiguous angle extraction), which is
exactly why this is a real Cargo test suite and not just asserted math.

```bash
cargo test                        # decomposition identities, QASM parser, optimizer
cargo run --example full_pipeline # parse -> decompose -> optimize -> fidelity -> run
```

All 18 tests and the example were run against the real `sirraya-qutub
0.1.13` pulled from crates.io (not a mock or a local copy) before this
was written out.

## Repo relationship

Kept as a separate crate with an ordinary crates.io version dependency
on `sirraya-qutub`, not folded into a workspace -- so it can version and
release independently.
