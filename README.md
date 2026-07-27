# sirraya-qutub-transpiler

[![Crates.io](https://img.shields.io/crates/v/sirraya-qutub-transpiler.svg)](https://crates.io/crates/sirraya-qutub-transpiler)
[![Documentation](https://docs.rs/sirraya-qutub-transpiler/badge.svg)](https://docs.rs/sirraya-qutub-transpiler)
[![License: MIT/Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-blue.svg)](https://www.rust-lang.org)

A QASM 2.0 importer and native-gate compiler for quantum circuits destined to run on the Sirraya QuTub ecosystem. This crate decouples circuit *description* from the specific gate set that `sirraya-qutub` was calibrated against, enabling circuits written in convenient gates (H, CX, T, RXX, etc.) to be compiled down to the operations that `HardwareCalibration` actually provides fidelity numbers for.

---

## Overview

The transpiler provides a complete pipeline for taking high-level quantum circuit descriptions and preparing them for execution on hardware:

- **Parse** OpenQASM 2.0 circuits into an intermediate representation
- **Decompose** arbitrary gates into the native gate set `{Rz, Ry, Rzz}`
- **Optimize** circuits through peephole optimization passes
- **Estimate** hardware fidelity based on realistic error models
- **Execute** compiled circuits on `QuantumRegister` instances

The crate depends on [`sirraya-qutub`](https://crates.io/crates/sirraya-qutub) straight from crates.io, with no prerequisite changes to that repository required.

---

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
sirraya-qutub-transpiler = "0.1.0"
```

---

## Pipeline

```
QASM Text → Parse → IR Circuit → Decompose → Native Circuit {Rz, Ry, Rzz} → Optimization Pass → (Fidelity Estimate OR Execution on QuantumRegister)

```

### 1. **QASM Parser**
A deliberately narrow OpenQASM 2.0 subset parser that handles the dialect `sirraya-qutub` writes (`h q[0];`, `rzz(1.2) q[0], q[2];`, etc.), plus standard mnemonics for the same gate set. The parser rejects gate definitions and classical control—any unsupported construct results in a parse error with line number, not silent skipping.

### 2. **Native Gate Decomposition**
Every gate is decomposed down to the native set `{Rz, Ry, Rzz}`:
- **Single-qubit rotations** via ZYZ Euler decomposition
- **Two-qubit gates** via exact identities built on `Rzz` (CNOT via CZ via Rzz; RXX/RYY via basis-changed Rzz)

This gate set is what `HardwareCalibration` provides single-qubit and two-qubit fidelity numbers for.

### 3. **Optimization Pass**
A peephole optimization pass that:
- Merges adjacent same-axis rotations on the same qubit(s)
- Drops rotations that cancel to approximately zero

### 4. **Fidelity Estimation**
A self-contained fidelity estimator that re-implements the published formula from the `sirraya-qutub` documentation (`p = (1-F)*d/(d-1)`) using the same published Quantinuum Helios numbers. This decoupling is deliberate—it allows independent validation and avoids depending on internal representation details of `sirraya-qutub`. See `src/fidelity.rs` for the full reasoning.

### 5. **Circuit Execution**
The emit module interfaces directly with `sirraya-qutub` to run `NativeCircuit` instances on real `QuantumRegister` instances and write native circuits back out as QASM.

---

## Usage Examples

### Basic Pipeline

```rust
use sirraya_qutub_transpiler::{
    QasmParser, Transpiler, gate_decomposition::decompose_circuit,
    fidelity::estimate_circuit_fidelity,
};

// Parse QASM 2.0 circuit
let qasm = r#"
    OPENQASM 2.0;
    include "qelib1.inc";
    qreg q[2];
    creg c[2];
    h q[0];
    cx q[0], q[1];
    measure q -> c;
"#;

let circuit = QasmParser::parse(qasm)?;

// Decompose to native gates {Rz, Ry, Rzz}
let decomposed = decompose_circuit(&circuit);

// Optimize
let optimized = Transpiler::optimize(&decomposed);

// Estimate fidelity (Quantinuum Helios)
let fidelity = estimate_circuit_fidelity(&optimized);
println!("Estimated fidelity: {:.2}%", fidelity * 100.0);
```

### Gate Decomposition Examples

```rust
// Single-qubit gates decompose to {Rz, Ry}
// H(0) → Rz(0, π) Ry(0, π/2)
// X(0) → Ry(0, π) Rz(0, -π)
// Rx(0, θ) → Rz(0, π/2) Ry(0, θ) Rz(0, -π/2)

// Two-qubit gates decompose to {Rzz} with single-qubit rotations
// CZ(0, 1) → Rz(0, π/2) Rz(1, π/2) Rzz(0, 1, -π/2)
// Rxx(0, 1, θ) → Rz... Rzz(0, 1, θ) Rz...
```

### Running the Full Pipeline

```bash
cargo run --example full_pipeline
```

### Viewing Gate Decompositions

```bash
cargo run --example gate_cheatsheet
```

---

## Supported Gates

### Single-Qubit Gates
| Gate | Native Decomposition |
|------|---------------------|
| H | Rz(π) Ry(π/2) |
| X | Ry(π) Rz(-π) |
| Y | Ry(π) |
| Z | Rz(π) |
| S | Rz(π/2) |
| T | Rz(π/4) |
| Rx(θ) | Rz(π/2) Ry(θ) Rz(-π/2) |
| Ry(θ) | Native |
| Rz(θ) | Native |

### Two-Qubit Gates
| Gate | Native Decomposition |
|------|---------------------|
| CX | 6 single-qubit + 1 Rzz |
| CZ | 2 single-qubit + 1 Rzz |
| SWAP | 18 single-qubit + 3 Rzz |
| Rxx(θ) | 8 single-qubit + 1 Rzz |
| Ryy(θ) | 12 single-qubit + 1 Rzz |
| Rzz(θ) | Native |
| Cp(θ) | 2 single-qubit + 1 Rzz |

---

## Performance Benchmarks

Fidelity estimation for GHZ state preparation on Quantinuum Helios (calibration: single-qubit error 5e-5, two-qubit error 1.05e-3):

| Qubits | Native Gates | Optimized Gates | Estimated Fidelity |
|--------|--------------|-----------------|-------------------|
| 2      | 8, 1         | 9               | 99.85%            |
| 4      | 20, 3        | 23              | 99.58%            |
| 8      | 44, 7        | 51              | 99.05%            |
| 16     | 92, 15       | 107             | 97.98%            |
| 32     | 188, 31      | 219             | 95.88%            |
| 64     | 380, 63      | 443             | 91.81%            |
| 98     | 584, 97      | 681             | 87.68%            |

*98 qubits matches Quantinuum Helios's own qubit count—a full-width GHZ state represents a worst-case scenario for this device with n-1 sequential two-qubit gates on the critical path.*

---

## Testing

Every decomposition identity is validated against `sirraya_qutub::core::QuantumRegister` directly. The test suite builds randomized input states, applies gates both directly and through the decomposed+optimized pipeline, then compares via `QuantumRegister::fidelity`. This catches even subtle errors like branch-ambiguous angle extraction in the ZYZ synthesizer.

```bash
cargo test                        # All tests (decomposition identities, QASM parser, optimizer)
cargo test -- --nocapture         # Show detailed output
cargo run --example full_pipeline # End-to-end pipeline demo
```

All tests are run against the real `sirraya-qutub` crate pulled from crates.io (not a mock or local copy) before each release.

---

## Repository Relationship

This crate is maintained as a **separate package** with an ordinary crates.io version dependency on `sirraya-qutub`, not folded into a workspace. This allows independent versioning and release cycles while maintaining a clear dependency boundary.

---

## Contributing

Contributions are welcome! Please ensure:

1. All tests pass (`cargo test`)
2. Documentation is updated for any new features
3. Examples are provided for significant additions
4. Breaking changes are clearly communicated

---

## License

This project is licensed under either of:

- MIT License ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

---

## Authors

**Sirraya Labs** - [amir@sirraya.org](mailto:amir@sirraya.org)

