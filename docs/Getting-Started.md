# Getting Started

Welcome to the Sirraya QuTub Transpiler.

This guide will help you install the project, build it from source, run the test suite, and execute the available examples.

---

# Prerequisites

Before building the project, ensure you have the following installed.

- Rust (stable toolchain)
- Cargo
- Git

Check your installation:

```bash
rustc --version
cargo --version
git --version
```

If Rust is not installed, visit:

https://rustup.rs

---

# Clone the Repository

Clone the repository from GitHub.

```bash
git clone https://github.com/sirraya/qutub-transpiler.git
```

Move into the project directory.

```bash
cd qutub-transpiler
```

---

# Build the Project

Compile the transpiler.

```bash
cargo build
```

For an optimized release build:

```bash
cargo build --release
```

---

# Run the Test Suite

The project contains an extensive collection of unit tests validating parsing, routing, decomposition, optimization, simulation, visualization, and compiler correctness.

Run every test:

```bash
cargo test
```

Run with output visible:

```bash
cargo test -- --nocapture
```

---

# Explore the Examples

The repository includes several standalone examples demonstrating different parts of the compiler.

List available examples:

```bash
cargo run --example
```

Run an example:

```bash
cargo run --example bell_state
```

Replace `bell_state` with any example located inside the `examples/` directory.

Examples illustrate different capabilities such as:

- Bell state preparation
- Quantum teleportation
- GHZ state generation
- QFT
- Grover search
- Routing
- Native decomposition
- OpenQASM parsing
- Circuit visualization
- Backend lowering
- Hardware-aware compilation

---

# Project Layout

The current source tree is intentionally compact.

```
examples/
src/
Cargo.toml
README.md
LICENSE
```

The compiler implementation currently resides directly inside the `src/` directory.

Major modules include:

- OpenQASM parsing
- Circuit representation
- Compiler passes
- Routing
- Native gate decomposition
- Backend lowering
- Hardware topology
- Pulse scheduling
- Quantum simulation
- Visualization
- Export utilities

---

# Documentation

Additional documentation is available throughout this guide.

- Introduction
- Architecture
- Compiler Pipeline
- Routing
- Native Decomposition
- Hardware Backends
- Pulse Scheduling
- Visualization
- Examples
- API Reference
- Contributing

---

# Development Workflow

Typical development follows the standard Rust workflow.

Build:

```bash
cargo build
```

Run tests:

```bash
cargo test
```

Format the code:

```bash
cargo fmt
```

Run Clippy:

```bash
cargo clippy
```

---

# Contributing

We welcome contributions from students, researchers, and engineers.

Good places to start include:

- Documentation improvements
- Additional examples
- Compiler optimizations
- Routing algorithms
- Backend support
- Pulse scheduling
- Testing
- Visualization
- Bug fixes

Please read the Contributing guide before opening a pull request.

---

# Need Help?

If you encounter an issue or have a question:

- Open a GitHub Issue
- Start a GitHub Discussion
- Read the project documentation
- Join the community discussions

We are happy to help new contributors get started.

---

# Next Steps

Once you have successfully built and tested the project, continue with:

- Compiler Architecture
- The Compilation Pipeline
- Intermediate Representation
- Routing
- Native Gate Decomposition
- Backend Lowering
- Pulse Scheduling
- Contributing