# qec_three_qubit_codes

A complete encode → error → syndrome → conditional correction → decode round for both three-qubit repetition codes.

```bash
cargo run --example qec_three_qubit_codes
```

## What it demonstrates

The example prepares logical `|1⟩`, encodes it into three physical data qubits, and injects an error on the middle qubit:

- `ThreeQubitBitFlipCode` receives `Gate::X(1)`.
- `ThreeQubitPhaseFlipCode` receives `Gate::Z(1)`.

Each code uses the `StabilizerCode` interface to measure two stabilizers into classical bits. A middle-qubit error produces syndrome `[1, 1]`. `correct()` appends three `Gate::If` branches, one for each non-zero syndrome; the simulator executes only the branch whose two classical conditions match the measured syndrome.

The example then calls `decode()` and verifies the recovered logical qubit against an independently prepared `|1⟩` density matrix. It exits with an error unless the syndrome is `[1, 1]` and recovery fidelity is `1.0` within numerical tolerance.

## Why the correction gates are conditional

Syndrome extraction diagnoses an error without directly measuring the encoded logical state. The classical result decides which physical correction is safe:

| Syndrome | Corrected data qubit |
|---|---|
| `[1, 0]` | q0 |
| `[1, 1]` | q1 |
| `[0, 1]` | q2 |
| `[0, 0]` | none |

The example prints the actual `Gate::If` table emitted by each code so the classical feed-forward step remains visible even though it is constructed through the shared trait.
