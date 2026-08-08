# Getting Started

This page is for cloning the repository, building the compiler from source, running its test suite, and exploring the bundled examples. If you just want to depend on the published crate in your own project, see [Installation](installation.md) instead.

## Prerequisites

* Rust (stable toolchain)
* Cargo
* Git

```bash
rustc --version
cargo --version
git --version
```

If Rust is not installed, visit [rustup.rs](https://rustup.rs).

## Clone the repository

```bash
git clone https://github.com/sirraya/qutub-transpiler.git
cd qutub-transpiler
```

## Build the project

```bash
cargo build
```

For an optimized release build:

```bash
cargo build --release
```

## Run the test suite

The project contains an extensive collection of unit tests validating parsing, routing, decomposition, optimization, simulation, visualization, and compiler correctness.

```bash
cargo test
```

Run with output visible:

```bash
cargo test -- --nocapture
```

## Explore the examples

```bash
cargo run --example
```

lists every available example. Run one with:

```bash
cargo run --example bell_state_end_to_end
```

For a guided tour — organized by what you're trying to learn, not just an alphabetical file list — see the [Examples](examples.md) page.

## Development workflow

```bash
cargo build          # build
cargo test            # run tests
cargo fmt              # format
cargo fmt -- --check   # verify formatting without changing files
cargo clippy -- -D warnings   # lint, treating warnings as errors
```

## Need help?

* Open a GitHub Issue
* Start a GitHub Discussion
* Read the rest of this documentation, starting with [Architecture](architecture.md)

## Next steps

Once you've built and tested the project, continue with [Architecture](architecture.md) to understand how each compiler pass works, or [Contributing](contributing.md) to submit a change.
