# Introduction

Sirraya QuTub is an open-source quantum computing ecosystem written in Rust. This crate, `sirraya-qutub-transpiler`, is its compiler layer: it transforms high-level, hardware-independent quantum circuits into representations suitable for execution, simulation, visualization, fidelity estimation, or export to external quantum platforms.

## Why a transpiler?

Quantum computers expose different native gate sets, connectivity constraints, execution models, and hardware characteristics. A quantum algorithm written once should be portable across these systems without requiring the algorithm itself to change.

The transpiler bridges this gap: it accepts a hardware-independent circuit and incrementally transforms it into a circuit that satisfies the requirements of whichever backend you target — without you having to hand-write a different circuit per device.

## Design philosophy

**Modularity.** Optimization, routing, decomposition, validation, and backend lowering are implemented independently, so each can evolve without affecting unrelated parts of the compiler. Rather than treating compilation as a single monolithic process, every stage is modeled independently — easier to understand, extend, test, and research.

**Hardware independence.** The compiler's internal representation is independent of any particular quantum computer. Hardware-specific details are introduced only during backend lowering — the parser never knows about hardware, the optimizer never knows about routing, the router never knows about pulse scheduling.

**Explicit transformations.** Compilation consists of explicit, understandable transformations rather than hidden side effects. Every compiler pass has clearly defined inputs, outputs, and responsibilities.

**Correctness first.** Compiler optimizations must preserve circuit semantics. Critical transformations are validated using simulation and numerical verification wherever possible — decompositions are checked against the real execution engine, not just asserted mathematically.

**Research-friendly.** Sirraya QuTub is designed as a platform for experimentation as much as a production compiler. Researchers can prototype new routing algorithms, optimization passes, decomposition strategies, scheduling techniques, and backend architectures without rewriting the rest of the compiler.

## Who this documentation is for

* **Users** — install the transpiler, compile quantum circuits, and execute programs on supported backends. Start with [Installation](installation.md).
* **Contributors** — understand the architecture, compiler passes, and contribution workflow. Start with [Getting Started](getting-started.md) and [Contributing](contributing.md).
* **Researchers** — explore the internal compiler design and experiment with new compilation techniques. Start with [Architecture](architecture.md).
* **Students** — learn how modern quantum compilers are structured and how quantum programs are transformed before execution. Start with [Architecture](architecture.md).

## Project goals

The long-term vision is a modular, extensible, open quantum compiler infrastructure capable of supporting a broad range of quantum hardware technologies — growing beyond circuit transformation into scheduling, calibration-aware compilation, pulse generation, hardware abstraction, and eventually direct integration with quantum control electronics.

Continue to [Installation](installation.md) to compile your first circuit.
