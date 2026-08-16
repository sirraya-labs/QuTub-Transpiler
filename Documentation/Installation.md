# Installation

This guide covers all ways to install the Sirraya QuTub Transpiler — from crates.io, from source, and as a development dependency.

---

## System Requirements

| Requirement | Minimum Version | Notes |
|-------------|-----------------|-------|
| Rust (rustc) | 1.70.0 | Stable toolchain only |
| Cargo | 1.70.0 | Bundled with Rustup |
| Git | 2.x | For cloning the repository |
| RAM | 2 GB | For building and running tests |
| Disk | ~200 MB | For source + build artifacts |

The transpiler runs on Linux, macOS, and Windows (via WSL2 recommended).

---

## Quick Install (crates.io)

Add the transpiler as a dependency in your `Cargo.toml`:

```toml
[dependencies]
sirraya-qutub-transpiler = "0.1.1"
```

Then build your project:

```bash
cargo build
```

The latest release is available on [crates.io](https://crates.io/crates/sirraya-qutub-transpiler) and documented on [docs.rs](https://docs.rs/sirraya-qutub-transpiler).

---

## Install Rust

If you don't have Rust installed, use [rustup](https://rustup.rs):

### Linux / macOS

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### Windows

Download and run [`rustup-init.exe`](https://win.rustup.rs/x86_64-pc-windows-msvc).

### Verify Installation

```bash
rustc --version
cargo --version
```

Both should report version 1.70.0 or higher.

---

## Build from Source

### Clone the Repository

```bash
git clone https://github.com/sirraya-labs/QuTub-Transpiler.git
cd QuTub-Transpiler
```

### Build

For a debug build (faster compilation, useful during development):

```bash
cargo build
```

For an optimized release build:

```bash
cargo build --release
```

The compiled library is available at `target/debug/libsirraya_qutub_transpiler.rlib` (debug) or `target/release/` (release).

### Run the Test Suite

Verify your installation by running all tests:

```bash
cargo test
```

A full test run covers parsing, routing, decomposition, optimization, simulation, visualization, and compiler correctness.

### Run Examples

```bash
cargo run --example bell_state
```

See the [examples directory](../examples/) for all available demonstrations.

---

## Install as a Development Dependency

To use the transpiler for local development and testing:

```toml
[dev-dependencies]
sirraya-qutub-transpiler = "0.1.1"
```

This ensures the transpiler is only compiled during `cargo test` and `cargo doc`, keeping your main build lean.

---

## Optional Features

The transpiler supports optional feature flags for extended functionality:

```toml
[dependencies]
sirraya-qutub-transpiler = { version = "0.1.1", features = ["qiskit"] }
```

Available features:

| Feature | Description |
|---------|-------------|
| `qiskit` | Qiskit backend integration for IBM hardware |
| `pyo3` | Python bindings via PyO3 |
| `serde` | Serialization support for circuit I/O |

---

## Troubleshooting

### Build fails with "rustc version too old"

Update your Rust toolchain:

```bash
rustup update stable
```

### Tests fail on Windows

WSL2 is recommended. Native Windows support is experimental — ensure you have the GNU toolchain installed.

### `cargo test` runs out of memory

Reduce parallel test execution:

```bash
cargo test -- --test-threads=2
```

### Dependency resolution conflicts

The transpiler depends on `sirraya-qutub = "0.1.13"`. If you have a conflicting version, run:

```bash
cargo update
```

---

## Next Steps

After installation, continue with:

- [Getting Started](Getting%20Started.md) — build, test, and run your first circuit
- [Compiler Architecture](Compiler%20Architecture/Readme.md) — understand how the transpiler works
- [Contributing](../Documentation/CONTRIBUTING.md) — join the community
