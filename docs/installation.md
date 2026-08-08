# Installation

This page is for using `sirraya-qutub-transpiler` as a dependency in your own Rust project. If you want to clone the repository, build the compiler from source, and run its test suite and examples, see [Getting Started](getting-started.md) instead.

## Add the crate

```toml
[dependencies]
sirraya-qutub-transpiler = "0.1"
```

The transpiler depends on the `sirraya-qutub` crate, also published on crates.io — no additional repositories or system dependencies are required. Requires Rust 1.70 or later.

## Verify your toolchain

```bash
rustc --version
cargo --version
```

If Rust isn't installed, get it from [rustup.rs](https://rustup.rs).

## Your first compilation

The following example parses an OpenQASM program, optimizes it, compiles it into native operations, and estimates its expected fidelity on real hardware:

```rust
use sirraya_qutub_transpiler::{
    qasm,
    optimize_ir,
    decompose,
    optimize,
    estimate_circuit_fidelity,
    PublishedCalibration,
};

let source = r#"
OPENQASM 2.0;
include "qelib1.inc";

qreg q[2];
creg c[2];

h q[0];
cx q[0], q[1];

measure q[0] -> c[0];
measure q[1] -> c[1];
"#;

let circuit = qasm::parse(source)?;
let circuit = optimize_ir(&circuit);

let native = decompose(&circuit);
let native = optimize(&native);

let calibration = PublishedCalibration::quantinuum_helios_2026();
let fidelity = estimate_circuit_fidelity(&native, &calibration);

println!("{:.2}%", fidelity * 100.0);
```

## Targeting a specific backend

The same logical circuit can also be lowered to a specific hardware backend and exported as IBM-compatible OpenQASM:

```rust
use sirraya_qutub_transpiler::{
    Backend,
    lower,
    qasm,
    optimize_ir,
    to_ibm_qasm,
};

let circuit = qasm::parse(source)?;
let circuit = optimize_ir(&circuit);

let backend = lower(&circuit, Backend::IbmQ);
let qasm = to_ibm_qasm(&backend, "bell_state")?;

std::fs::write("bell.qasm", qasm)?;
```

The exported file is compatible with IBM Quantum's native OpenQASM workflow and can be submitted directly or integrated into a Qiskit pipeline.

## Next steps

* Want to see the full range of backends and native gate sets? See [Architecture](architecture.md).
* Want to build the compiler from source and run the bundled examples? See [Getting Started](getting-started.md).
