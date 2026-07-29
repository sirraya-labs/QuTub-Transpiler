# Sirraya QuTub Transpiler — Architecture

> **A hardware-aware quantum compiler boundary between circuit semantics and executable reality.**

Sirraya QuTub Transpiler is an open-source quantum compilation system developed by **Sirraya Labs**.

Its purpose is to transform quantum programs from a high-level representation into circuits that are valid, optimized, hardware-aware, and independently verifiable for a target execution environment.

The project is intentionally designed as a compiler rather than a collection of gate-conversion utilities. Its architecture separates **quantum program semantics**, **compiler transformations**, **physical constraints**, **backend capabilities**, **calibration information**, and **verification** so that each layer can evolve independently.

The long-term objective is to provide a rigorous foundation for quantum compilation research and practical execution while remaining understandable, testable, and extensible for an open-source community.

---

## Table of Contents

* [1. Architecture at a Glance](#1-architecture-at-a-glance)
* [2. Why a Quantum Compiler Needs Multiple Layers](#2-why-a-quantum-compiler-needs-multiple-layers)
* [3. Design Goals](#3-design-goals)
* [4. Architectural Principles](#4-architectural-principles)
* [5. Compilation Pipeline](#5-compilation-pipeline)
* [6. Frontend and Parsing](#6-frontend-and-parsing)
* [7. Intermediate Representation](#7-intermediate-representation)
* [8. Compiler Analyses](#8-compiler-analyses)
* [9. Compiler Passes](#9-compiler-passes)
* [10. Source-Level Optimization](#10-source-level-optimization)
* [11. Gate Synthesis and Native Decomposition](#11-gate-synthesis-and-native-decomposition)
* [12. Placement and Layout](#12-placement-and-layout)
* [13. Routing](#13-routing)
* [14. Backend Architecture](#14-backend-architecture)
* [15. Hardware and Calibration Model](#15-hardware-and-calibration-model)
* [16. Cost Models](#16-cost-models)
* [17. Noise- and Fidelity-Aware Compilation](#17-noise--and-fidelity-aware-compilation)
* [18. Native-Level Optimization](#18-native-level-optimization)
* [19. Scheduling](#19-scheduling)
* [20. Parameterized and Symbolic Circuits](#20-parameterized-and-symbolic-circuits)
* [21. Pauli Algebra and Hamiltonian Infrastructure](#21-pauli-algebra-and-hamiltonian-infrastructure)
* [22. Verification and Correctness](#22-verification-and-correctness)
* [23. Measurement Verification](#23-measurement-verification)
* [24. Diagnostics and Compilation Reports](#24-diagnostics-and-compilation-reports)
* [25. Reproducibility](#25-reproducibility)
* [26. Performance Engineering](#26-performance-engineering)
* [27. Error Handling](#27-error-handling)
* [28. Extending QuTub](#28-extending-qutub)
* [29. Adding a New Gate](#29-adding-a-new-gate)
* [30. Adding a New Optimization Pass](#30-adding-a-new-optimization-pass)
* [31. Adding a New Backend](#31-adding-a-new-backend)
* [32. Testing Strategy](#32-testing-strategy)
* [33. Benchmarking](#33-benchmarking)
* [34. Architecture Decision Records](#34-architecture-decision-records)
* [35. Repository Organization](#35-repository-organization)
* [36. Current Architecture vs. Target Architecture](#36-current-architecture-vs-target-architecture)
* [37. Long-Term Direction](#37-long-term-direction)
* [38. Contributing to the Architecture](#38-contributing-to-the-architecture)
* [39. Guiding Principle](#39-guiding-principle)

---

# 1. Architecture at a Glance

The simplest description of QuTub is:

```text
High-level quantum program
            |
            v
      Frontend / Parser
            |
            v
     Semantic Compiler IR
            |
            v
          Analysis
            |
            v
   Source-level Optimization
            |
            v
      Logical Placement
            |
            v
          Routing
            |
            v
   Native Gate Synthesis
            |
            v
      Backend Lowering
            |
            v
    Native-level Optimization
            |
            v
 Calibration / Noise Analysis
            |
            v
        Scheduling
            |
            v
        Verification
            |
            v
    Validated Native Circuit
            |
            +------------------+
            |                  |
            v                  v
       Compilation         Execution /
          Report            Simulation
```

The key architectural boundary is:

> **The logical circuit describes what should happen. The target backend describes what can happen. The compiler connects the two.**

---

# 2. Why a Quantum Compiler Needs Multiple Layers

A quantum circuit can be mathematically correct and still be a poor circuit to execute.

For example, two logically equivalent circuits may differ significantly in:

* native gate count,
* two-qubit gate count,
* circuit depth,
* physical connectivity requirements,
* SWAP overhead,
* execution duration,
* calibration quality,
* expected error,
* and scheduling constraints.

A compiler therefore has to solve several distinct problems.

### Semantic problem

What operation does the circuit represent?

### Mathematical problem

Can the circuit be transformed into an equivalent but simpler form?

### Architectural problem

Which physical qubits should represent the logical qubits?

### Routing problem

How can required interactions be made physically executable?

### Synthesis problem

How can abstract gates be expressed using the target's native operations?

### Hardware problem

Which physical operations are currently reliable and available?

### Timing problem

When can each operation actually execute?

### Verification problem

How do we know that the compiler preserved the intended computation?

QuTub treats these as related but separable concerns.

---

# 3. Design Goals

QuTub is designed around the following goals.

## 3.1 Correctness

Compiler transformations must preserve circuit semantics within explicitly defined numerical or statistical tolerances.

Correctness takes precedence over optimization.

---

## 3.2 Extensibility

New:

* gates,
* optimization passes,
* routing algorithms,
* synthesis strategies,
* hardware models,
* backends,
* cost functions,
* verification methods,

should be addable without redesigning unrelated parts of the compiler.

---

## 3.3 Hardware awareness

The architecture must be capable of representing physical constraints rather than assuming an ideal all-to-all-connected quantum computer.

---

## 3.4 Explicitness

Important assumptions should be represented explicitly in types, configuration, diagnostics, and documentation.

The compiler should not silently:

* discard unsupported operations,
* change measurement semantics,
* introduce approximation,
* assume connectivity,
* or substitute a backend.

---

## 3.5 Reproducibility

Compilation results should be reproducible whenever deterministic algorithms are selected.

Randomized algorithms should support explicit seeds.

---

## 3.6 Verifiability

Important compiler transformations should have a corresponding correctness strategy.

---

## 3.7 Research friendliness

The architecture should support experimental compiler algorithms without forcing experimental behavior into the stable compilation path.

---

## 3.8 Long-term maintainability

The project is intended to be developed over many years by contributors who may not have participated in its earliest implementation.

Architecture must therefore communicate intent, not merely implementation.

---

# 4. Architectural Principles

## 4.1 Separate semantics from implementation

The IR should represent what a circuit means without prematurely encoding how one particular backend executes it.

---

## 4.2 Separate logical and physical qubits

A logical qubit represents the program's identity.

A physical qubit represents a location on a target system.

These concepts must not be conflated.

```text
Logical q0
    |
    | placement
    v
Physical Q7
```

A later routing decision may change the mapping without changing the logical program.

---

## 4.3 Prefer exact transformations

When an exact identity exists, it should generally be preferred over approximation.

Approximate synthesis is valid when explicitly requested or selected by a compilation policy and must expose its error tolerance.

---

## 4.4 Optimize according to explicit objectives

There is no universal definition of the "best" quantum circuit.

Depending on the target, users may care about:

* gate count,
* two-qubit gate count,
* depth,
* latency,
* expected fidelity,
* estimated error,
* SWAP count,
* or a weighted combination.

These objectives belong in explicit cost models.

---

## 4.5 Verification is part of compilation

Verification should not be an afterthought.

The compiler should make it possible to validate transformations systematically.

---

## 4.6 Diagnostics are part of the interface

A mature compiler should eventually be able to explain:

> Why was this gate introduced?

> Why was this route selected?

> Why was this decomposition chosen?

> Which optimization pass changed the circuit?

Explainability is particularly important when optimization becomes multi-objective.

---

# 5. Compilation Pipeline

The target compilation architecture is:

```mermaid
flowchart TD
    A["Quantum Program"] --> B["Frontend / Parser"]
    B --> C["Semantic IR"]
    C --> D["Analysis"]
    D --> E["Source Optimization"]
    E --> F["Placement / Layout"]
    F --> G["Routing"]
    G --> H["Native Gate Synthesis"]
    H --> I["Backend Lowering"]
    I --> J["Native Optimization"]
    J --> K["Calibration / Noise Analysis"]
    K --> L["Scheduling"]
    L --> M["Verification"]
    M --> N["Validated Native Circuit"]
    N --> O["Simulation / Execution"]
    N --> P["Compilation Report"]
```

The exact set and order of passes may evolve.

The architecture should therefore support configurable pass pipelines rather than requiring one immutable sequence.

---

# 6. Frontend and Parsing

The frontend translates an external quantum program representation into QuTub's internal representation.

The current project includes an OpenQASM 2.0 parser.

The frontend is responsible for:

* lexical and syntactic validation,
* gate recognition,
* register handling,
* qubit references,
* measurement representation,
* parameter parsing,
* useful source-location diagnostics.

Unsupported constructs should produce explicit errors.

They should never be silently ignored.

---

## 6.1 Frontend independence

The compiler core should not depend on one source language.

Future frontends may include:

* OpenQASM,
* programmatic Rust APIs,
* circuit interchange formats,
* higher-level quantum representations,
* research-specific input formats.

All should converge into the compiler's semantic representation.

```text
OpenQASM ───────┐
                |
Rust API ───────┼──> Compiler IR
                |
Other frontend ┘
```

---

# 7. Intermediate Representation

The IR is the central contract between compiler stages.

A useful IR must represent:

* quantum operations,
* logical qubits,
* classical bits,
* measurements,
* resets,
* parameters,
* operation ordering,
* metadata,
* source locations,
* and eventually regions or basic blocks where needed.

The IR should be expressive enough to represent the program without requiring backend-specific decisions.

---

## 7.1 Logical operations

Examples include:

```text
H(q0)
X(q1)
Rx(q2, θ)
CX(q0, q1)
Rzz(q1, q2, φ)
Measure(q0)
Reset(q1)
```

---

## 7.2 Native operations

After target lowering, the representation may contain operations such as:

```text
Rz
Ry
Rzz
```

or another backend-defined native gate set.

The compiler should preserve enough metadata to determine where each operation came from.

---

## 7.3 Parameter representation

Long-term support should distinguish:

```text
Concrete:
    Rz(1.570796)

Symbolic:
    Rz(theta)

Expression:
    Rz(theta + phi)
```

This is important for parameterized circuits and variational algorithms.

---

# 8. Compiler Analyses

Analyses compute information that passes can consume without modifying the circuit.

Important analyses include:

### Dependency analysis

Determines which operations depend on previous operations.

### Depth analysis

Computes logical and physical circuit depth.

### Critical-path analysis

Identifies operations that constrain execution time.

### Resource analysis

Counts:

* logical qubits,
* physical qubits,
* gates,
* one-qubit gates,
* two-qubit gates,
* measurements,
* resets,
* depth.

### Interaction graph

Represents relationships between logical qubits.

```text
Vertex = logical qubit
Edge   = interaction
```

### Commutation analysis

Determines whether operations can be reordered while preserving semantics.

---

# 9. Compiler Passes

QuTub should evolve toward a pass-oriented architecture.

Conceptually:

```text
Pass Manager
    |
    +-- Analysis Passes
    |
    +-- Transformation Passes
    |
    +-- Placement Passes
    |
    +-- Routing Passes
    |
    +-- Synthesis Passes
    |
    +-- Scheduling Passes
    |
    +-- Verification Passes
    |
    +-- Reporting Passes
```

Each transformation pass should document:

* inputs,
* outputs,
* assumptions,
* invariants,
* analyses required,
* analyses invalidated,
* configuration,
* expected complexity,
* verification strategy.

---

## 9.1 Pass composition

Passes should be composable.

For example:

```text
Parse
  ↓
Canonicalize
  ↓
Optimize
  ↓
Analyze
  ↓
Place
  ↓
Route
  ↓
Decompose
  ↓
Optimize
  ↓
Schedule
  ↓
Verify
```

A future backend or optimization profile should be able to select a different sequence without modifying individual passes.

---

# 10. Source-Level Optimization

Source-level optimization operates on semantic gates before hardware constraints dominate the representation.

Examples include:

```text
H H       → I
X X       → I
Z Z       → I
Rz(a)Rz(b) → Rz(a+b)
```

Other future transformations may include:

* gate cancellation,
* rotation merging,
* commutation,
* block simplification,
* Clifford identities,
* Pauli simplification,
* redundant operation removal.

---

## 10.1 Why optimize before routing?

Routing can introduce additional operations.

Therefore it is useful to simplify the logical circuit before introducing physical constraints.

However, optimization should also happen after routing and decomposition because those stages can expose new opportunities.

This naturally produces:

```text
Source Optimization
        ↓
Routing
        ↓
Synthesis
        ↓
Native Optimization
```

rather than one optimization pass.

---

# 11. Gate Synthesis and Native Decomposition

The compiler must eventually translate abstract operations into operations supported by the target.

For a target supporting:

```text
{Rz, Ry, Rzz}
```

a higher-level operation may be represented using those primitives.

Examples include:

```text
H
X
Rx
CX
CZ
SWAP
Rxx
Ryy
```

The exact decomposition belongs to the synthesis layer rather than being hard-coded into unrelated compiler components.

---

## 11.1 Synthesis properties

Every synthesis rule should specify:

* source operation,
* native target set,
* exact or approximate status,
* mathematical identity,
* parameter conditions,
* expected native cost,
* verification method.

---

## 11.2 Technology-specific synthesis

Different technologies expose different native operations.

Therefore:

```text
Logical Gate
      |
      v
Synthesis Strategy
      |
      +----> Backend A
      |
      +----> Backend B
      |
      +----> Backend C
```

The compiler should not assume that one decomposition is universally optimal.

---

# 12. Placement and Layout

Placement maps logical qubits to physical qubits.

```mermaid
flowchart LR
    A["Logical Interaction Graph"] --> B["Placement Strategy"]
    B --> C["Physical Hardware Graph"]
    C --> D["Initial Logical-to-Physical Mapping"]
    D --> E["Routing"]
```

Placement can consider:

* connectivity,
* interaction frequency,
* expected routing overhead,
* physical gate fidelity,
* physical qubit availability,
* execution duration.

---

## 12.1 Interaction graph

For a circuit containing:

```text
CX(q0,q1)
CX(q1,q2)
CX(q0,q2)
```

the interaction graph contains:

```text
q0 ─── q1
 \     /
   q2
```

This graph can be compared with the target hardware topology to find useful initial placements.

---

# 13. Routing

Routing transforms logical interactions into physically executable interactions.

For example, if:

```text
Logical:
q0 ─── q2
```

but the hardware only supports:

```text
Q0 ─── Q1 ─── Q2
```

the compiler must move logical state or otherwise transform the circuit so that the interaction becomes executable.

SWAP insertion is one mechanism.

---

## 13.1 Routing strategies

The architecture should permit multiple strategies, including:

* shortest-path routing,
* A* search,
* lookahead routing,
* SABRE-style approaches,
* noise-aware routing,
* fidelity-aware routing,
* research algorithms.

No single routing algorithm should be treated as universally optimal.

---

## 13.2 Routing cost

A routing algorithm may optimize:

```text
SWAP count
+
depth
+
two-qubit error
+
duration
+
critical-path impact
```

The selected objective should be explicit.

---

# 14. Backend Architecture

A backend describes what a target can execute.

A backend may define:

* physical qubit count,
* native gate set,
* connectivity,
* gate durations,
* gate fidelities,
* measurement properties,
* reset behavior,
* calibration information,
* scheduling constraints,
* target-specific lowering rules.

The compiler core should interact with these capabilities through stable abstractions.

---

## 14.1 Backend independence

The architecture should permit:

```text
                 Compiler Core
                /      |      \
               /       |       \
          Backend A Backend B Backend C
```

A backend should not require rewriting the compiler's semantic layers.

---

# 15. Hardware and Calibration Model

A target hardware model should eventually represent physical characteristics.

## Qubit properties

Potential properties include:

* T1,
* T2,
* frequency,
* readout error,
* availability,
* calibration status.

## Gate properties

Potential properties include:

* native operation,
* fidelity,
* error rate,
* duration,
* calibration timestamp,
* physical qubit pair.

## Connectivity

The backend should represent which interactions are supported.

---

## 15.1 Calibration snapshots

Calibration information should be versioned and associated with compilation.

A compilation artifact should ideally be traceable to:

```text
Compiler version
Backend version
Calibration snapshot
Compilation profile
Source hash
Configuration
Random seed
```

This is essential for reproducibility.

---

# 16. Cost Models

A compiler needs a formal answer to:

> What does "better" mean?

A simple cost model might be:

```text
C =
    wg × gate_count
  + wd × depth
  + ws × swap_count
  + we × estimated_error
  + wt × duration
```

where each weight represents the importance of an objective.

The actual implementation may use more sophisticated models.

The key architectural principle is that cost should be **explicit and replaceable**.

---

## 16.1 Possible optimization profiles

QuTub may eventually expose profiles such as:

```text
GateCount
Depth
Fidelity
Latency
Balanced
Research
Debug
```

Each profile can select different:

* passes,
* routing algorithms,
* synthesis strategies,
* cost functions,
* verification levels.

---

# 17. Noise- and Fidelity-Aware Compilation

A circuit with fewer gates is not necessarily a better circuit.

For example:

```text
Circuit A
7 gates
2 high-error two-qubit operations

Circuit B
9 gates
1 high-error two-qubit operation
```

A gate-count optimizer may prefer A.

A fidelity-aware optimizer may prefer B.

QuTub's architecture should allow the objective to be selected according to the target and user requirements.

---

## 17.1 Error budget

A future compilation report may expose:

```text
Estimated Error Budget

Single-qubit operations      14%
Two-qubit operations         68%
Readout                       18%
```

This should be clearly labeled as an estimate.

The compiler must distinguish:

* measured hardware values,
* calibration-derived values,
* theoretical models,
* assumptions,
* approximations.

---

# 18. Native-Level Optimization

Optimization should happen again after synthesis.

This is important because decomposition may create new patterns.

For example:

```text
Logical circuit
      ↓
Native decomposition
      ↓
Rz(a)
Rz(b)
      ↓
Rz(a+b)
```

Native-level optimization may include:

* adjacent rotation merging,
* cancellation,
* commutation,
* redundant operation elimination,
* backend-specific identities,
* duration-aware transformations.

---

# 19. Scheduling

Scheduling assigns execution times to operations.

Potential scheduling strategies include:

* ASAP,
* ALAP,
* critical-path scheduling,
* duration-aware scheduling,
* resource-constrained scheduling,
* hardware-aware scheduling.

Scheduling must respect:

* dependencies,
* qubit availability,
* gate durations,
* hardware constraints,
* measurement/reset behavior,
* timing restrictions.

---

## 19.1 Why scheduling belongs after compilation

The final operation set and physical qubit assignment affect execution timing.

Therefore scheduling generally becomes more meaningful after:

```text
Routing
+
Synthesis
+
Backend lowering
```

although earlier scheduling analyses may still be useful.

---

# 20. Parameterized and Symbolic Circuits

Quantum algorithms frequently use parameterized circuits.

Examples include:

```text
Rx(theta)
Rz(phi)
Rzz(theta + phi)
```

The compiler should eventually represent symbolic expressions without requiring them to be evaluated immediately.

This enables:

* variational algorithms,
* parameter sweeps,
* symbolic simplification,
* parameter-aware optimization,
* reusable compiled templates.

---

## 20.1 Symbolic simplification

For example:

```text
Rz(theta)
Rz(phi)
```

can become:

```text
Rz(theta + phi)
```

without knowing the numerical values of either parameter.

---

# 21. Pauli Algebra and Hamiltonian Infrastructure

A long-term compiler can benefit from representations of Pauli operators:

```text
I
X
Y
Z
```

and Pauli strings such as:

```text
X0 Z1 Y3
```

This creates infrastructure for:

* Clifford transformations,
* Pauli rotations,
* Hamiltonians,
* VQE,
* QAOA,
* measurement grouping,
* symbolic operator manipulation.

---

## 21.1 Hamiltonian representation

A Hamiltonian can be represented conceptually as:

```text
H = Σ ci Pi
```

where `Pi` is a Pauli string.

For example:

```text
H =
    0.5 Z0
  + 0.7 Z1
  - 0.2 X0 X1
```

A future Hamiltonian layer may support:

* addition,
* subtraction,
* scalar multiplication,
* Pauli multiplication,
* coefficient merging,
* simplification,
* grouping.

---

## 21.2 Hamiltonian evolution

A synthesis layer may eventually support:

```text
exp(-iHt)
```

using methods such as:

* Pauli evolution,
* Trotterization,
* Suzuki formulas,
* commuting-group evolution,
* approximate synthesis.

Approximation must always expose its tolerance.

---

# 22. Verification and Correctness

Correctness is one of the defining engineering properties of QuTub.

The compiler should verify transformations at the strongest practical level.

---

## 22.1 Exact equivalence

For sufficiently small circuits, compare exact or numerically evaluated representations where practical.

---

## 22.2 Randomized state verification

A general strategy is:

```text
1. Generate a randomized initial state.
2. Execute the reference circuit.
3. Execute the transformed circuit.
4. Compare the resulting states.
```

For operations where fidelity is an appropriate metric:

```text
abs(fidelity - 1.0) < tolerance
```

should be used with a documented tolerance.

---

## 22.3 Property-based testing

Property-based testing can generate families of circuits and verify invariants automatically.

Potential properties include:

```text
U × U† = I
```

or:

```text
compile(compile(C)) ≈ compile(C)
```

where such an invariant is valid for the particular compilation stage.

---

## 22.4 Differential testing

Where appropriate, QuTub can compare against independent implementations.

This is particularly useful for:

* gate identities,
* parser behavior,
* measurement,
* synthesis,
* numerical algorithms.

---

# 23. Measurement Verification

Measurement requires different verification techniques because measurement changes the state and produces statistical outcomes.

Useful approaches include:

* repeated-shot testing,
* distribution comparison,
* statistical hypothesis tests,
* confidence intervals,
* controlled random seeds where appropriate.

A measurement transformation should be evaluated statistically rather than forced into a unitary equivalence framework.

---

# 24. Diagnostics and Compilation Reports

A mature compiler should make compilation observable.

A compilation report may contain:

```text
Sirraya QuTub Compilation Report

Input
    Logical qubits:          32
    Operations:             412
    Logical depth:           87

Placement
    Physical qubits:         32
    Mapping:                 complete

Routing
    SWAPs inserted:           19
    Physical depth:          121

Synthesis
    Native 1Q operations:    614
    Native 2Q operations:    108

Optimization
    Operations removed:       73
    Rotations merged:         41

Target
    Backend:                 <target>
    Calibration:             <snapshot>

Execution estimate
    Duration:                <estimate>
    Fidelity:                <estimate>

Verification
    Equivalence:             PASS
```

The exact report format may evolve.

---

## 24.1 Explainable compilation

A future compiler should be able to answer questions such as:

```text
Why was this SWAP inserted?

Why was this physical qubit selected?

Why was this synthesis rule selected?

Which optimization removed this operation?

What caused circuit depth to increase?

Which physical interactions dominate estimated error?
```

Explainability should become increasingly important as compiler decisions become more complex.

---

# 25. Reproducibility

A compilation result should ideally be reproducible from:

```text
Source program
+
Compiler version
+
Backend
+
Calibration snapshot
+
Compilation profile
+
Configuration
+
Random seed
```

This is particularly important for:

* research,
* benchmarking,
* debugging,
* regression testing,
* scientific publication.

---

# 26. Performance Engineering

Quantum compilation can become computationally expensive.

Potential bottlenecks include:

* IR allocation,
* graph construction,
* dependency analysis,
* routing search,
* symbolic manipulation,
* synthesis,
* verification,
* memory usage.

Performance improvements should be driven by profiling.

Correctness should not be sacrificed for premature optimization.

---

# 27. Error Handling

Errors should be explicit and typed where appropriate.

Possible error categories include:

```text
UnsupportedGate
InvalidCircuit
InvalidQubit
InvalidMapping
RoutingError
SynthesisError
BackendError
CalibrationError
VerificationError
NumericalError
ConfigurationError
```

The compiler should fail loudly rather than produce a plausible but incorrect circuit.

Unsupported operations must not be silently skipped.

---

# 28. Extending QuTub

A contribution should ideally have a clear architectural home.

Before implementing a new capability, determine whether it belongs to:

```text
Frontend
IR
Analysis
Optimization
Placement
Routing
Synthesis
Backend
Calibration
Scheduling
Verification
Reporting
```

If a feature does not fit cleanly into one of these boundaries, that may indicate that the architecture itself needs discussion.

---

# 29. Adding a New Gate

A new gate should generally require:

```text
1. Define its semantic representation.
2. Define validation rules.
3. Define parser support if applicable.
4. Define decomposition or native support.
5. Document mathematical identities.
6. Add direct tests.
7. Add randomized equivalence tests where appropriate.
8. Add regression cases.
9. Update examples/documentation.
10. Benchmark if the gate materially affects compilation cost.
```

A gate should not be considered complete merely because it can be parsed.

---

# 30. Adding a New Optimization Pass

A new optimization should document:

```text
Purpose
Preconditions
Transformation
Semantic guarantee
Affected IR
Required analyses
Invalidated analyses
Complexity
Numerical considerations
Verification strategy
Benchmark results
```

For example:

```text
Pass:
    RotationMerge

Input:
    Rz(a), Rz(b)

Output:
    Rz(a+b)

Requirement:
    Same logical qubit

Guarantee:
    Exact up to numerical representation
```

---

# 31. Adding a New Backend

A backend should describe at minimum:

```text
Native gates
Physical qubits
Connectivity
Gate durations
Gate fidelity/error information
Measurement behavior
Reset behavior
Calibration representation
Scheduling constraints
```

A new backend should include:

* backend tests,
* example circuits,
* verification tests,
* documentation,
* benchmarks where practical.

Backend-specific assumptions should remain localized to the backend abstraction.

---

# 32. Testing Strategy

Testing should occur at multiple levels.

## Unit tests

Validate individual functions and data structures.

## Identity tests

Validate mathematical transformations.

## Integration tests

Validate complete compiler stages.

## End-to-end tests

Validate:

```text
Input
→ Parse
→ Compile
→ Execute / Simulate
→ Verify
```

## Property-based tests

Generate broad circuit families.

## Regression tests

Every important bug should become a permanent test.

---

## 32.1 Reference simulation

Where appropriate, the real `sirraya-qutub` simulator should be used as the semantic reference rather than relying only on mocked behavior.

This is particularly important for decomposition correctness.

---

# 33. Benchmarking

Benchmarking should measure more than compilation time.

Relevant metrics include:

```text
Compilation time
Gate count
1Q gate count
2Q gate count
Depth
SWAP count
Estimated duration
Estimated fidelity
Estimated error
Memory consumption
Verification cost
```

Benchmarks should include diverse workloads:

* small circuits,
* wide circuits,
* deep circuits,
* routing-heavy circuits,
* parameterized circuits,
* algorithmic circuits,
* noise-sensitive circuits.

An optimization should be evaluated against a clearly stated objective.

---

# 34. Architecture Decision Records

Major architectural decisions should be recorded in ADRs.

Suggested structure:

```text
docs/
  adr/
    0001-ir-design.md
    0002-pass-manager.md
    0003-routing-interface.md
    0004-hardware-model.md
    0005-fidelity-model.md
```

Each ADR should contain:

```text
Context
Problem
Decision
Alternatives
Rationale
Consequences
Status
```

ADRs preserve architectural history and prevent future contributors from having to reconstruct decisions from source code and git history.

---

# 35. Repository Organization

As the project grows, a structure along these lines may be appropriate:

```text
sirraya-qutub-transpiler/
│
├── src/
│   ├── ir/
│   ├── frontend/
│   ├── analysis/
│   ├── passes/
│   ├── optimize/
│   ├── placement/
│   ├── routing/
│   ├── synthesis/
│   ├── backend/
│   ├── calibration/
│   ├── scheduling/
│   ├── verification/
│   ├── diagnostics/
│   └── reporting/
│
├── tests/
│   ├── parser/
│   ├── identities/
│   ├── optimization/
│   ├── routing/
│   ├── synthesis/
│   ├── backend/
│   ├── verification/
│   └── integration/
│
├── examples/
│
├── benchmarks/
│
├── docs/
│   ├── adr/
│   ├── design/
│   └── research/
│
├── ARCHITECTURE.md
├── CONTRIBUTING.md
├── CODE_OF_CONDUCT.md
└── README.md
```

This is a target organizational model rather than a requirement that the current repository immediately adopt every directory.

---

# 36. Current Architecture vs. Target Architecture

QuTub is an evolving open-source project.

It is important to distinguish between **implemented capabilities** and **architectural direction**.

## Current foundation

The project currently provides the foundations for:

* OpenQASM 2.0 parsing,
* circuit IR,
* native decomposition,
* source-level optimization,
* routing and coupling maps,
* backend lowering,
* native optimization,
* fidelity estimation,
* QASM emission,
* execution through `sirraya-qutub`,
* and direct simulator-based verification.

## Target architecture

The long-term architecture described in this document includes additional capabilities such as:

* a richer compiler IR,
* modular pass management,
* reusable analysis infrastructure,
* advanced placement,
* multiple routing strategies,
* hardware-aware cost models,
* calibration-aware compilation,
* noise-aware optimization,
* scheduling,
* symbolic compilation,
* Pauli and Hamiltonian infrastructure,
* richer verification,
* explainable compilation,
* and broader backend support.

These are architectural directions and should not be interpreted as claims that every capability is already implemented.

The repository, release notes, examples, and API documentation should remain the authoritative sources for current feature availability.

---

# 37. Long-Term Direction

The long-term evolution of QuTub can be understood as a progression:

```mermaid
flowchart TD
    A["Current Foundation"] --> B["Compiler Infrastructure"]
    B --> C["Analysis + Optimization"]
    C --> D["Placement + Routing"]
    D --> E["Hardware Awareness"]
    E --> F["Calibration + Noise Awareness"]
    F --> G["Scheduling"]
    G --> H["Symbolic Compilation"]
    H --> I["Quantum Algorithm Infrastructure"]
    I --> J["Advanced Research Compiler"]
```

The stages are not rigid release commitments.

They represent architectural maturity.

---

## 37.1 Stage 1 — Compiler infrastructure

Strengthen:

* IR,
* pass manager,
* analyses,
* diagnostics,
* testing,
* documentation.

---

## 37.2 Stage 2 — Advanced optimization

Develop:

* commutation,
* identity registries,
* Clifford optimization,
* symbolic simplification,
* multi-objective cost models.

---

## 37.3 Stage 3 — Physical compilation

Develop:

* placement,
* topology abstractions,
* routing strategies,
* hardware models,
* backend-specific synthesis.

---

## 37.4 Stage 4 — Hardware-aware compilation

Develop:

* calibration snapshots,
* gate durations,
* physical fidelity,
* error models,
* noise-aware routing,
* error budgets.

---

## 37.5 Stage 5 — Scheduling

Develop:

* timing-aware compilation,
* critical-path optimization,
* resource-aware scheduling,
* backend timing constraints.

---

## 37.6 Stage 6 — Symbolic and algorithmic infrastructure

Develop:

* symbolic parameters,
* Pauli algebra,
* Hamiltonians,
* Hamiltonian evolution,
* measurement grouping.

---

## 37.7 Stage 7 — Research platform

Explore:

* new synthesis algorithms,
* new routing algorithms,
* approximate compilation,
* calibration-adaptive compilation,
* advanced verification,
* multi-objective optimization,
* automated compiler search,
* hardware-specific research.

---

# 38. Contributing to the Architecture

QuTub is an open-source project.

Architecture is therefore a shared engineering artifact rather than something owned exclusively by the original authors.

Contributors are encouraged to propose improvements when they can demonstrate:

* a real problem,
* a clear architectural boundary,
* a measurable benefit,
* a correctness strategy,
* and a maintainable implementation.

For substantial changes, open a discussion or issue before beginning implementation.

This is particularly important for changes involving:

* IR design,
* backend interfaces,
* routing,
* synthesis,
* optimization semantics,
* calibration,
* scheduling,
* public APIs.

Small fixes and clearly scoped improvements generally do not require prior design discussion.

---

## 38.1 What makes a strong architectural contribution?

A strong contribution does more than add code.

It explains:

```text
Problem
    ↓
Constraints
    ↓
Design
    ↓
Alternatives
    ↓
Trade-offs
    ↓
Implementation
    ↓
Verification
    ↓
Benchmark
```

This makes the contribution useful to future engineers even after the original author is no longer maintaining it.

---

## 38.2 Research contributions

Research-oriented work is welcome.

A research contribution should ideally provide:

* a clear description of the algorithm,
* references where appropriate,
* assumptions,
* experimental methodology,
* reproducible configuration,
* benchmark circuits,
* results,
* limitations.

Research code may remain experimental until sufficient validation exists.

---

# 39. Guiding Principle

The central principle of QuTub can be expressed simply:

> **A quantum circuit is not finished when it is mathematically valid. It is finished when it has been transformed into a circuit that the target execution system can execute efficiently, reliably, and verifiably.**

That requires the compiler to understand several things at once:

```text
                Quantum Semantics
                       |
                       v
                  Mathematics
                       |
                       v
                 Compiler IR
                       |
                       v
                   Analysis
                       |
                       v
                  Optimization
                       |
                       v
                Physical Topology
                       |
                       v
                    Routing
                       |
                       v
                Native Synthesis
                       |
                       v
                 Hardware Model
                       |
                       v
              Calibration + Noise
                       |
                       v
                   Scheduling
                       |
                       v
                  Verification
                       |
                       v
              Executable Circuit
```

The long-term vision is therefore not simply:

```text
QASM → gates
```

It is:

```text
Quantum Program
      ↓
Semantic Understanding
      ↓
Compiler Analysis
      ↓
Mathematical Optimization
      ↓
Physical Mapping
      ↓
Hardware-Aware Routing
      ↓
Technology-Aware Synthesis
      ↓
Calibration-Aware Optimization
      ↓
Execution Scheduling
      ↓
Independent Verification
      ↓
Validated Execution Artifact
```

Sirraya QuTub Transpiler is intended to grow into a **general, hardware-aware, verification-oriented quantum compilation framework** while remaining open to multiple quantum technologies, compiler strategies, research directions, and contributors.

The architecture should evolve.

The principles should remain clear.

And every significant transformation should be explainable, testable, and grounded in the semantics of the quantum computation it transforms.
