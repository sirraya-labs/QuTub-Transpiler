# Frequently Asked Questions

This page answers common questions about using and contributing to **QuTub Transpiler**.

## Installation & Setup

### Which Rust version do I need?

QuTub Transpiler requires **Rust 1.70 or later**. Install it via [rustup.rs](https://rustup.rs):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Verify your installation:

```bash
rustc --version  # 1.70.0 or later
cargo --version  # 1.70.0 or later
```

### Do I need to install anything else?

You only need:
- **Rust** (1.70+) and **Cargo** (comes with Rust)
- **Git** for cloning the repository

No system dependencies, no Docker, no external quantum hardware is required. The compiler works entirely offline once cloned.

## Building & Testing

### How do I build the project?

```bash
cargo build
```

For an optimized release build:

```bash
cargo build --release
```

### How do I run the tests?

```bash
cargo test
```

Run with output visible:

```bash
cargo test -- --nocapture
```

Run a specific test:

```bash
cargo test <test_name>
```

### How do I run the linter?

```bash
cargo clippy -- -D warnings
```

This treats all warnings as errors, matching the project's CI configuration.

### How do I format my code?

```bash
cargo fmt
```

Verify formatting without changing files:

```bash
cargo fmt -- --check
```

### Do I need to run all checks before submitting a PR?

Yes. Before opening a pull request, ensure:

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
```

## Using the Transpiler

### How do I compile a QASM 2.0 circuit?

```rust
use sirraya_qutub_transpiler::qasm;

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
```

### How do I optimize a circuit?

```rust
use sirraya_qutub_transpiler::optimize_ir;

let optimized = optimize_ir(&circuit);
```

### How do I decompose to native gates?

```rust
use sirraya_qutub_transpiler::decompose;

let native = decompose(&circuit);
```

### How do I estimate circuit fidelity?

```rust
use sirraya_qutub_transpiler::{estimate_circuit_fidelity, PublishedCalibration};

let calibration = PublishedCalibration::quantinuum_helios_2026();
let fidelity = estimate_circuit_fidelity(&native, &calibration);

println!("{:.2}%", fidelity * 100.0);
```

### How do I lower to a specific backend?

```rust
use sirraya_qutub_transpiler::{Backend, lower, to_ibm_qasm};

let backend = lower(&circuit, Backend::IbmQ);
let qasm = to_ibm_qasm(&backend, "bell_state")?;
```

## Contributing

### Do I need to be a quantum computing expert?

**No.** The project welcomes contributions of all kinds:

- Bug reports and bug fixes
- Documentation improvements
- Test coverage
- Design discussions
- Optimization work
- Backend development

You do not need quantum computing expertise to contribute meaningfully.

### What counts as a "good first issue"?

Look for issues labeled `good first issue`. These are typically:

- Documentation improvements (like this page)
- Test coverage additions
- Small bug fixes
- Example additions

See [CONTRIBUTING](contributing.md) for the full workflow.

### What coding style should I follow?

```bash
cargo fmt          # Format your code
cargo clippy -- -D warnings  # Lint with warnings-as-errors
cargo test         # Run tests
```

All three must pass before your PR will be reviewed.

### What commit message format should I use?

Use conventional commit messages:

```
feat: add support for new gate
fix: correct routing logic for linear chain
docs: update installation instructions
test: add fidelity test for GHZ state
```

## Troubleshooting

### `cargo build` fails with "cannot find crate `sirraya-qutub`"

Make sure you're building from the repository root, not from a subdirectory. The crate is defined in `Cargo.toml` at the project root.

### `cargo test` fails with timeout

Some tests run simulations that can take a while. Try running with a longer timeout:

```bash
cargo test -- --test-threads=1
```

Or run a specific test:

```bash
cargo test <test_name>
```

### I get an error about missing `qelib1.inc`

This file is part of the QASM 2.0 standard library. If you're parsing circuits that include it, make sure the file is on your include path. The parser looks for it relative to the input file by default.

### My circuit's fidelity is lower than expected

Fidelity depends on:
1. The target hardware's calibration (gate times, error rates)
2. The circuit's depth and gate count
3. The compiler's optimization and routing decisions

Try:
- Reducing circuit depth where possible
- Using `optimize_ir()` before decomposing
- Checking that you're using the correct `PublishedCalibration` for your target hardware

### I can't find a specific error message

Search the source code for the error message text:

```bash
grep -r "error message" src/
```

If you still can't find it, open an issue with the full error output and your circuit.

## Still have questions?

- Open a [GitHub Issue](https://github.com/sirraya-labs/QuTub-Transpiler/issues)
- Start a [Discussion](https://github.com/sirraya-labs/QuTub-Transpiler/discussions)
- Read the [Architecture](architecture.md) doc for deep technical details
- Read the [Contributing](contributing.md) doc for how to submit changes