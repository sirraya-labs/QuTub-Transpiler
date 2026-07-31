# Introduction

Welcome to the **Sirraya QuTub Transpiler** documentation.

Sirraya QuTub is an open-source quantum compiler written in Rust that transforms quantum programs into executable circuits for different quantum hardware architectures. It provides a modular compilation pipeline that separates parsing, optimization, routing, decomposition, backend lowering, validation, and execution into well-defined stages.

Rather than treating compilation as a single monolithic process, Sirraya QuTub models each stage independently, making the compiler easier to understand, extend, test, and research.

---

## Why Sirraya QuTub?

Quantum computers expose different native gate sets, connectivity constraints, execution models, and hardware characteristics. A quantum algorithm written once should be portable across these systems without requiring the algorithm itself to change.

The transpiler bridges this gap.

It accepts a hardware-independent quantum circuit and incrementally transforms it into a circuit that satisfies the requirements of the selected execution backend.

---

## Compilation Pipeline

The compiler currently follows a modular pipeline:

```
OpenQASM
    │
    ▼
Parser
    │
    ▼
Intermediate Representation (IR)
    │
    ▼
Source Optimization
    │
    ▼
Logical-to-Physical Routing
    │
    ▼
Native Gate Decomposition
    │
    ▼
Backend Lowering
    │
    ▼
Native Optimization
    │
    ▼
Validation
    │
    ▼
Execution
```

Each stage has a clearly defined responsibility and communicates through explicit data structures rather than hidden assumptions.

---

## Design Philosophy

The architecture is guided by several principles.

### Modularity

Every compiler pass should solve one problem well.

Optimization, routing, decomposition, validation, and backend lowering are implemented independently so they can evolve without affecting unrelated parts of the compiler.

---

### Hardware Independence

The compiler internally represents circuits independently of any particular quantum computer.

Hardware-specific details are introduced only during backend lowering.

---

### Explicit Transformations

Compilation should consist of explicit, understandable transformations rather than hidden side effects.

Each compiler pass should have clearly defined inputs, outputs, and responsibilities.

---

### Correctness First

Compiler optimizations must preserve circuit semantics.

Critical transformations are validated using simulation and numerical verification wherever possible.

---

### Research-Friendly Architecture

Sirraya QuTub is designed not only as a production compiler but also as a platform for experimentation.

Researchers can prototype new routing algorithms, optimization passes, decomposition strategies, scheduling techniques, and backend architectures without rewriting the rest of the compiler.

---

## Current Capabilities

The project currently includes support for:

- OpenQASM parsing
- Intermediate Representation (IR)
- Source-level optimization
- Topology-aware qubit routing
- Native gate decomposition
- Backend abstraction
- Circuit validation
- Quantum circuit visualization
- Multiple hardware targets
- IBM Qiskit export
- Comprehensive testing

The architecture is continuously expanding as new compiler passes and hardware capabilities are implemented.

---

## Who Is This Documentation For?

This documentation is intended for several audiences.

### Users

Learn how to install the transpiler, compile quantum circuits, and execute programs on supported backends.

### Contributors

Understand the architecture, compiler passes, project structure, and contribution workflow.

### Researchers

Explore the internal compiler design and experiment with new compilation techniques.

### Students

Learn how modern quantum compilers are structured and how quantum programs are transformed before execution.

---

## Documentation Structure

This documentation is organized into several sections.

- **Getting Started** — Installation and first compilation.
- **Architecture** — High-level compiler design.
- **Compiler Pipeline** — Detailed explanation of every compilation stage.
- **Intermediate Representation** — Circuit representation and semantic model.
- **Routing** — Logical-to-physical qubit mapping.
- **Optimization** — Source and native optimization passes.
- **Pulse Scheduling** *(planned)* — Hardware control layer.
- **Hardware Backends** — Target-specific compilation.
- **API Reference** — Public Rust API.
- **Contributing** — Development workflow and coding guidelines.
- **Roadmap** — Upcoming features and long-term vision.

---

## Project Goals

The long-term vision of Sirraya QuTub is to provide a modular, extensible, and open quantum compiler infrastructure capable of supporting a broad range of quantum hardware technologies.

As the ecosystem evolves, the compiler is intended to grow beyond circuit transformation into scheduling, calibration-aware compilation, pulse generation, hardware abstraction, and eventually direct integration with quantum control electronics.

---

Continue to the next section to learn how to install the compiler and compile your first quantum circuit.