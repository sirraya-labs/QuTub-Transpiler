# Installation

This guide explains how to install and use **QuTub Transpiler** in your own Rust project.

If you want to clone the repository, compile the transpiler from source, run its tests, or explore the bundled examples, follow the [Getting Started](getting-started.md) guide instead.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Install Rust](#install-rust)
- [Create a Rust Project](#create-a-rust-project)
- [Add QuTub Transpiler](#add-qutub-transpiler)
- [Verify the Installation](#verify-the-installation)
- [Your First Compilation](#your-first-compilation)
- [Targeting a Specific Backend](#targeting-a-specific-backend)
- [Troubleshooting](#troubleshooting)
- [Next Steps](#next-steps)

## Prerequisites

Before using QuTub Transpiler, make sure you have:

| Requirement | Description |
|---|---|
| Rust | Stable Rust toolchain, version 1.70 or later |
| Cargo | Rust package manager and build tool (included with Rust) |
| Internet connection | Required to download the crate and its dependencies |

You can check whether Rust and Cargo are installed by running:

```bash
rustc --version
cargo --version
```

Both commands should display their installed versions.

## Install Rust

QuTub Transpiler is written in Rust. If Rust is not installed, install it using the official Rust toolchain installer, **rustup**.

1. Visit [https://rustup.rs](https://rustup.rs).
2. Download the installer for your operating system.
3. Run the installer and follow the on-screen instructions.
4. Restart your terminal after installation.

Verify the installation:

```bash
rustc --version
cargo --version
rustup --version
```

Make sure your Rust compiler version is at least 1.70.

For additional installation instructions, refer to the [official Rust installation documentation](https://www.rust-lang.org/tools/install).

### Windows requirements

On Windows, the Rust MSVC toolchain requires the Microsoft C++ build tools and Windows SDK.

If you encounter linker-related errors during compilation, install the following components through Visual Studio Installer:

- MSVC C++ x64/x86 build tools.
- Windows 10 or Windows 11 SDK.

Restart your terminal after installing these components.

## Create a Rust Project

If you do not already have a Rust project, create one using Cargo:

```bash
cargo new my_quantum_project
```

Move into the project directory:

```bash
cd my_quantum_project
```

The generated project has the following basic structure:

```text
my_quantum_project/
├── Cargo.toml
└── src/
    └── main.rs
```

Cargo.toml contains project metadata and dependency declarations, while `src/main.rs` contains the application entry point.

## Add QuTub Transpiler

QuTub Transpiler is published on [crates.io](https://crates.io/crates/sirraya-qutub-transpiler).

Open your project's `Cargo.toml` file and add the dependency under `[dependencies]`:

```toml
[dependencies]
sirraya-qutub-transpiler = "0.1"
```

The dependency declaration allows Cargo to download a compatible release from the 0.1.x series.

The crate uses the `sirraya-qutub` package as a dependency. Cargo resolves and downloads its required dependencies automatically.

You do not need to clone the QuTub Transpiler repository when using the published crate.

## Verify the Installation

After adding the dependency, run the following command from your project directory:

```bash
cargo check
```

Cargo will resolve the dependency versions, download the required packages if necessary, and check whether your project compiles.

To perform a complete build, run:

```bash
cargo build
```

For an optimized release build, use:

```bash
cargo build --release
```

If these commands finish successfully, QuTub Transpiler has been added to your project correctly.

## Your First Compilation

The following example demonstrates how to parse an OpenQASM 2.0 program, optimize its intermediate representation, decompose it into native operations, and estimate its expected fidelity using a published hardware calibration.

Add the following code to `src/main.rs`:

```rust
use sirraya_qutub_transpiler::{
    qasm,
    optimize_ir,
    decompose,
    optimize,
    estimate_circuit_fidelity,
    PublishedCalibration,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Define an OpenQASM 2.0 quantum circuit.
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

    // Parse the OpenQASM program.
    let circuit = qasm::parse(source)?;

    // Optimize the intermediate representation.
    let circuit = optimize_ir(&circuit);

    // Decompose the circuit into native operations.
    let native = decompose(&circuit);

    // Optimize the resulting native operations.
    let native = optimize(&native);

    // Load a published hardware calibration.
    let calibration = PublishedCalibration::quantinuum_helios_2026();

    // Estimate the expected circuit fidelity.
    let fidelity = estimate_circuit_fidelity(&native, &calibration);

    println!("Estimated circuit fidelity: {:.2}%", fidelity * 100.0);

    Ok(())
}
```

Run the program:

```bash
cargo run
```

The program parses and processes the circuit, then prints the estimated circuit fidelity.

The displayed fidelity is an estimate based on the selected published calibration, not a measurement obtained by executing the circuit on physical quantum hardware.

## Targeting a Specific Backend

QuTub Transpiler can also lower a logical circuit to a specific backend.

For example, the following code demonstrates how to target the IBM Quantum backend and export the resulting circuit as OpenQASM:

```rust
use sirraya_qutub_transpiler::{
    Backend,
    lower,
    qasm,
    optimize_ir,
    to_ibm_qasm,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = r#"
        OPENQASM 2.0;
        include "qelib1.inc";

        qreg q[2];
        creg c[2];

        h q[0];
        cx q[0], q[1];
    "#;

    // Parse and optimize the circuit.
    let circuit = qasm::parse(source)?;
    let circuit = optimize_ir(&circuit);

    // Lower the circuit to the IBM Quantum backend.
    let backend = lower(&circuit, Backend::IbmQ);

    // Export the backend circuit as IBM-compatible OpenQASM.
    let qasm_output = to_ibm_qasm(&backend, "bell_state")?;

    // Save the generated circuit.
    std::fs::write("bell.qasm", qasm_output)?;

    println!("Circuit successfully exported to bell.qasm");

    Ok(())
}
```

Run the program:

```bash
cargo run
```

After successful execution, the generated `bell.qasm` file will be available in your project's root directory.

The exported circuit can be used in compatible IBM Quantum and Qiskit workflows.

## Troubleshooting

### 1. `rustc` or `cargo` is not recognized

**Cause:** Rust is not installed correctly, or its executable directory is missing from the system PATH.

**Solution:**

- Install Rust using [rustup.rs](https://rustup.rs).
- Restart your terminal or IDE.
- Verify the installation:

```bash
rustc --version
cargo --version
```

### 2. Dependency could not be downloaded

**Cause:** Cargo cannot access crates.io or download the required dependencies.

**Solution:**

- Check your internet connection.
- Retry the command:

```bash
cargo build
```

- If you are behind a proxy or firewall, check whether access to crates.io is permitted.

### 3. Linker errors on Windows

**Cause:** The required Microsoft C++ build tools or Windows SDK may be missing or unavailable to the compiler.

**Solution:**

- Open Visual Studio Installer.
- Ensure that MSVC C++ x64/x86 build tools are installed.
- Ensure that a Windows SDK is installed.
- Restart your terminal and try building again.

### 4. Rust version is too old

**Cause:** Your installed Rust toolchain does not meet the minimum supported version.

**Solution:**

Update the stable toolchain:

```bash
rustup update stable
rustup default stable
```

Then verify:

```bash
rustc --version
```

### 5. Build errors after changing dependencies

**Cause:** Cargo may be using previously generated build artifacts.

**Solution:**

Clean the project's build directory and rebuild:

```bash
cargo clean
cargo build
```

If the error persists, check the complete compiler output and ensure that the dependency declaration in `Cargo.toml` is correct.

## Next Steps

Once QuTub Transpiler is installed, explore the following resources:

- [Getting Started](getting-started.md) — Learn how to clone and build the repository from source.
- [Examples](examples.md) — Explore practical examples and supported workflows.
- [Architecture](architecture.md) — Understand the transpiler's internal architecture and compiler passes.
- [Contributing](contributing.md) — Learn how to contribute to the project.

You can also visit the [QuTub Transpiler GitHub repository](https://github.com/sirraya-labs/QuTub-Transpiler) for the latest source code and project updates.
