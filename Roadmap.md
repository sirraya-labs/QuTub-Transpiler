

# Sirraya QuTub Transpiler

## Long-Term Engineering Reference

**Organization:** Sirraya Labs
**Project:** Sirraya QuTub Transpiler
**Document type:** Engineering Architecture & Long-Term Development Reference
**Intended lifetime:** Multi-year
**Audience:** Compiler engineers, quantum software engineers, researchers, backend engineers, maintainers, and future technical leads

---

# 1. Purpose

Sirraya QuTub Transpiler should be understood as much more than a QASM parser or gate-conversion library.

Its long-term purpose is to become a **hardware-aware quantum compilation system** that transforms a high-level quantum program into a validated circuit appropriate for a specific execution target.

The fundamental problem is:

> A quantum circuit can be mathematically correct while still being a poor circuit to execute on real hardware.

A compiler therefore needs to reason about both **what a circuit means** and **how that circuit will behave when executed on a particular target system**.

The long-term system should evolve toward:

**Quantum Program → Semantic IR → Analysis → Optimization → Placement → Routing → Synthesis → Backend Lowering → Calibration-Aware Optimization → Scheduling → Verification → Validated Execution Artifact**

---

# 2. System Identity

The QuTub Transpiler is a compiler boundary between:

* how a quantum circuit is described,
* how it is represented internally,
* how it can be mathematically optimized,
* how logical qubits map to physical qubits,
* how gates are decomposed,
* how hardware constraints influence compilation,
* how noise and calibration influence decisions,
* and how the final circuit is verified before execution.

The target architecture is:

```text
Quantum Program
      |
      v
Frontend / Parser
      |
      v
Quantum Compiler IR
      |
      v
Analysis
      |
      v
Source Optimization
      |
      v
Placement / Layout
      |
      v
Routing
      |
      v
Technology-Aware Synthesis
      |
      v
Backend Lowering
      |
      v
Native Optimization
      |
      v
Calibration / Noise-Aware Optimization
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
      +----> Compilation Report
      |
      v
Simulation / Execution
```

The architecture must preserve a strict separation between:

**Semantic representation**
What the circuit means.

**Compiler representation**
How the compiler reasons about the circuit.

**Physical representation**
How a particular target can execute the circuit.

---

# 3. Architectural North Star

The long-term architecture can be understood as five major layers.

### Layer 1 — Understanding

The compiler receives a quantum program and converts it into an internal representation.

### Layer 2 — Reasoning

The compiler analyzes dependencies, interactions, resources, commutation, depth, and other properties.

### Layer 3 — Transformation

The compiler changes the circuit while preserving semantics.

### Layer 4 — Physicalization

The compiler adapts the abstract circuit to physical hardware topology, native gates, calibration, noise, and timing.

### Layer 5 — Trust

The compiler verifies the transformation and produces evidence explaining what happened.

This gives the system a fundamental principle:

> **QuTub should eventually understand not only what a quantum circuit is, but what it will cost to execute.**

---

# 4. Current System Baseline

The current system already establishes the foundation for this architecture.

Its conceptual pipeline is:

```text
QASM 2.0
   ↓
IR Circuit
   ↓
Source Optimization
   ↓
Routing
   ↓
Native Decomposition
   ↓
Backend Lowering
   ↓
Native Optimization
   ↓
Native Circuit
   ↓
sirraya-qutub
```

Current capabilities include:

* OpenQASM 2.0 parsing
* intermediate representation
* source-level optimization
* coupling maps
* routing
* SWAP insertion
* native gate decomposition
* backend-specific lowering
* native optimization
* fidelity estimation
* QASM emission
* execution through `sirraya-qutub`
* verification against the real simulator

This is the **foundation**, not the final architecture.

---

# 5. Core Engineering Principles

## 5.1 Correctness before optimization

The compiler must never sacrifice semantic correctness merely to reduce gate count or execution time.

The preferred priority is:

```text
Correctness
    >
Numerical stability
    >
Predictability
    >
Performance
    >
Aggressive optimization
```

A transformation that cannot be reliably verified should not be accepted merely because it improves a benchmark.

---

## 5.2 Prefer exact transformations

When an exact identity exists, prefer it.

For example:

```text
Rz(a) Rz(b)
      ↓
Rz(a + b)
```

is exact.

If approximation is introduced, it must be explicit.

For example:

```text
ApproximateSynthesis
    tolerance = ...
```

Approximation must never happen silently.

---

## 5.3 Hardware awareness must become first-class

The compiler should eventually understand that:

```text
3 gates on excellent hardware
```

can be better than:

```text
2 gates on poor hardware
```

Therefore gate count cannot be the only optimization objective.

---

## 5.4 Optimization objectives must be explicit

Possible objectives include:

* gate count
* two-qubit gate count
* circuit depth
* execution duration
* estimated fidelity
* expected error
* SWAP count
* critical path
* memory usage
* balanced cost

The compiler should use explicit cost models rather than hidden assumptions.

---

## 5.5 Verification is part of compilation

Verification must not be treated as a separate activity performed only after development.

Each significant transformation should have an appropriate verification strategy.

---

## 5.6 Diagnostics are part of the product

A mature compiler must eventually be able to answer:

> Why was this SWAP inserted?

> Why was this decomposition chosen?

> Why was this route selected?

> Which pass reduced the circuit?

> Which pass increased depth?

> Where is the estimated error concentrated?

Explainability becomes increasingly important as the compiler becomes sophisticated.

---

# 6. Intermediate Representation

The IR is one of the most important long-term investments in QuTub.

The current gate-oriented representation is a good starting point, but a mature compiler should eventually represent:

* circuits
* modules
* logical qubits
* physical qubits
* classical bits
* operations
* instructions
* blocks
* regions
* parameters
* symbolic expressions
* measurements
* resets
* barriers
* metadata
* gate definitions

The IR should support both concrete and parameterized operations.

For example:

```text
Rx(theta)
Rzz(phi)
```

should be representable even if `theta` and `phi` are not known during compilation.

---

# 7. Symbolic and Parameterized Compilation

The compiler should eventually understand expressions such as:

```text
theta
phi
theta + phi
theta - π/2
2 * theta
```

This enables symbolic optimization.

For example:

```text
Rz(theta)
Rz(phi)
```

can become:

```text
Rz(theta + phi)
```

without requiring numerical values.

The compiler should distinguish:

* concrete values
* symbolic parameters
* symbolic expressions
* unknown values

This is particularly important for variational quantum algorithms.

---

# 8. Pass Manager

As the compiler grows, passes should no longer be hard-coded into a single large pipeline.

A future architecture should support a pass manager conceptually similar to:

```text
Pass Manager
    |
    +-- Analysis Pass
    +-- Optimization Pass
    +-- Placement Pass
    +-- Routing Pass
    +-- Synthesis Pass
    +-- Scheduling Pass
    +-- Verification Pass
    +-- Reporting Pass
```

Each pass should define:

* its inputs
* its assumptions
* its outputs
* analyses it requires
* analyses it invalidates
* configuration
* statistics
* correctness guarantees

This will allow the compiler to grow without becoming one enormous transpilation function.

---

# 9. Analysis Framework

The compiler should eventually maintain reusable analyses.

Important analyses include:

### Dependency analysis

Determines which operations depend on previous operations.

### Depth analysis

Calculates logical and physical circuit depth.

### Critical-path analysis

Identifies operations that determine execution time.

### Resource analysis

Counts:

* logical qubits
* physical qubits
* gates
* one-qubit gates
* two-qubit gates
* measurements
* resets
* circuit depth

### Interaction graph

Represents:

```text
Vertex = logical qubit
Edge = interaction between logical qubits
```

This graph becomes extremely useful for placement and routing.

### Commutation analysis

Determines whether operations can be reordered without changing circuit semantics.

---

# 10. Dependency DAG

The compiler should eventually maintain an explicit dependency graph.

For example:

```text
H(q0)
   |
   v
CX(q0,q1)
   |
   v
Rz(q1)
   |
   v
CX(q1,q2)
```

Operations acting on independent qubits can potentially execute in parallel.

This DAG becomes the foundation for:

* scheduling
* depth analysis
* critical-path analysis
* optimization
* routing lookahead
* parallel execution

---

# 11. Optimization Architecture

Optimization should operate at multiple levels.

## Level 1 — Source optimization

Operate on semantic gates.

Examples:

```text
H H → I
X X → I
Z Z → I
Rz(a) Rz(b) → Rz(a+b)
```

## Level 2 — Structural optimization

Use:

* dependencies
* commutation
* DAG transformations
* block simplification

## Level 3 — Clifford optimization

Recognize Clifford structures and apply exact transformations.

For example:

```text
H X H = Z

H Z H = X
```

## Level 4 — Native optimization

Optimize again after backend decomposition.

This is important because a circuit that is elegant at the logical level can become inefficient after being translated into native gates.

---

# 12. Commutation Engine

Commutation should become a dedicated compiler service.

The engine should reason about:

```text
Can A commute through B?
```

Potential answers may depend on:

* whether the gates act on disjoint qubits
* known algebraic identities
* parameter values
* symbolic conditions
* backend-specific rules

The compiler must never infer commutation merely from gate names.

---

# 13. Identity Registry

A centralized identity system can become a major optimization foundation.

Identities may be categorized into:

* single-qubit identities
* two-qubit identities
* rotation identities
* Clifford identities
* Pauli identities
* backend-specific identities
* approximate identities

Each identity should record:

* pattern
* replacement
* conditions
* exact or approximate status
* mathematical justification
* verification method
* expected cost impact

This creates a systematic bridge between quantum mathematics and compiler optimization.

---

# 14. Pauli Algebra

A future version should introduce explicit representations for:

```text
I
X
Y
Z
```

and multi-qubit Pauli strings such as:

```text
X0 Z1 Y3
```

This enables:

* Clifford propagation
* Pauli rotations
* Hamiltonians
* VQE
* QAOA
* measurement grouping
* error analysis
* symbolic transformations

---

# 15. Hamiltonian Representation

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

The compiler could eventually support:

* addition
* scalar multiplication
* Pauli multiplication
* simplification
* coefficient merging
* grouping
* measurement grouping

This begins to move QuTub upward from a gate compiler toward a broader quantum programming infrastructure.

---

# 16. Hamiltonian Evolution

A future synthesis layer could support:

```text
exp(-iHt)
```

using techniques such as:

* Pauli evolution
* first-order Trotterization
* higher-order Suzuki formulas
* commuting-group evolution
* approximate synthesis

Any approximation must expose its tolerance and error characteristics.

---

# 17. Placement

Placement determines where logical qubits should initially live on physical hardware.

Conceptually:

```text
Logical Circuit
      ↓
Interaction Graph
      ↓
Physical Topology
      ↓
Initial Placement
      ↓
Routing
      ↓
Physical Circuit
```

Placement should eventually consider:

* expected SWAP count
* connectivity
* two-qubit fidelity
* gate duration
* critical-path impact

---

# 18. Topology Abstraction

Physical connectivity should be represented as a graph:

```text
G = (V, E)
```

where:

* `V` represents physical qubits
* `E` represents allowed interactions

Possible topologies include:

* linear chains
* rings
* grids
* heavy-hex
* all-to-all
* custom research topologies
* backend-specific topologies

The compiler must never assume full connectivity unless the target explicitly declares it.

---

# 19. Routing

Routing is the process of making logical interactions physically executable.

The system should eventually support several routing strategies:

* shortest-path routing
* A* routing
* lookahead routing
* SABRE-style routing
* noise-aware routing
* fidelity-aware routing

Routing should be a pluggable strategy rather than one permanently hard-coded algorithm.

---

# 20. Multi-Objective Routing

Routing decisions should eventually consider:

```text
SWAP count
+
Gate count
+
Depth
+
Two-qubit error
+
Execution duration
+
Critical path
```

A conceptual cost function is:

```text
C =
    wg × gate_count
  + wd × depth
  + ws × swap_count
  + we × estimated_error
  + wt × duration
```

The weights should be configurable.

---

# 21. Hardware Model

Hardware information should eventually become a first-class object.

A target hardware model should represent:

### Qubit properties

* T1
* T2
* frequency
* readout error
* availability

### Gate properties

* native operation
* fidelity
* duration
* error rate
* calibration metadata

### Connectivity

* available physical interactions
* directionality where applicable
* topology constraints

This allows compilation decisions to use real target characteristics.

---

# 22. Calibration Snapshots

Calibration should be versioned and timestamped.

A compilation result should be traceable to the calibration snapshot used during compilation.

For example:

```text
Compiler version
Backend
Calibration snapshot
Compilation profile
Configuration
Source hash
Random seed
```

This is essential for reproducibility.

---

# 23. Noise-Aware Compilation

The compiler should eventually optimize for expected execution quality rather than only circuit size.

Consider:

```text
Circuit A
7 gates
2 noisy two-qubit gates

Circuit B
9 gates
1 noisy two-qubit gate
```

A gate-count optimizer may select A.

A noise-aware compiler may select B.

Neither is universally correct.

The compilation profile should determine the objective.

---

# 24. Error Budget

Instead of reporting only:

```text
Estimated fidelity: 96.3%
```

a mature compiler should explain the estimated error.

For example:

```text
Estimated Error Budget

Single-qubit operations      12%
Two-qubit operations         71%
Readout                       17%

Primary contributors:
    physical pair A-B
    physical pair C-D
    readout on qubit 7
```

The system must distinguish between:

* measured values
* calibration-derived values
* theoretical estimates
* assumptions
* approximations

An estimate must never be presented as a measured hardware result.

---

# 25. Fidelity Estimation

The current fidelity estimator should evolve into a modular framework.

The architecture should distinguish:

```text
FidelityModel
ErrorModel
CalibrationSnapshot
CircuitStatistics
Estimator
```

This allows future models to be introduced without rewriting the compiler.

---

# 26. Scheduling

Scheduling turns the compiled circuit into timed operations.

Possible scheduling modes include:

* ASAP
* ALAP
* critical-path aware
* duration-aware
* resource-constrained
* calibration-aware

The scheduler should account for:

* operation duration
* qubit availability
* dependencies
* physical constraints
* timing restrictions

---

# 27. Verification

Verification is one of the most important differentiators of the system.

## Exact equivalence

For small circuits, compare the resulting unitary where practical.

## Randomized state verification

Generate randomized initial states and compare output states.

## Fidelity verification

For state simulation:

```text
abs(fidelity - 1.0) < tolerance
```

The tolerance should be centralized.

## Property-based testing

Generate random circuits and check semantic preservation.

## Differential testing

Compare against independent implementations where useful.

## Metamorphic testing

Verify known invariants such as:

```text
U × U† = I
```

---

# 28. Measurement Verification

Measurement cannot be validated in exactly the same way as unitary operations.

Instead, use:

* repeated shots
* probability distributions
* statistical tests
* confidence intervals
* controlled random seeds when appropriate

The test must define acceptable statistical error.

---

# 29. Testing Philosophy

The fundamental QuTub testing principle should remain:

> Every important decomposition, identity, routing transformation, and optimization pass should be validated against a trusted semantic reference whenever practical.

For a decomposition:

```text
1. Generate randomized initial state.
2. Execute the original operation.
3. Execute the decomposed operation.
4. Compare the results.
```

For measurement:

```text
1. Execute reference circuit repeatedly.
2. Execute compiled circuit repeatedly.
3. Compare resulting distributions.
```

For routing:

```text
1. Verify connectivity.
2. Verify logical-to-physical mapping.
3. Verify semantic equivalence.
4. Verify final mapping.
```

---

# 30. Regression Corpus

Every important bug should ideally become a permanent regression test.

The corpus should eventually include:

* identity circuits
* single-gate circuits
* randomized circuits
* Clifford circuits
* parameterized circuits
* Bell states
* GHZ states
* QFT
* Grover
* Shor components
* variational circuits
* Hamiltonian circuits
* measurement-heavy circuits
* routing stress tests
* deep circuits
* wide circuits
* connectivity stress tests
* noise-sensitive circuits

The regression corpus is a long-term engineering asset.

---

# 31. Compilation Reports

A mature compiler should explain what it did.

A report could contain:

```text
Sirraya QuTub Compilation Report

Input
    Qubits:                 32
    Gates:                  412
    Logical depth:           87

Placement
    Physical qubits:         32
    Initial mapping:       complete

Routing
    SWAPs inserted:          19
    Physical depth:         121

Decomposition
    Native 1Q gates:        614
    Native 2Q gates:        108

Optimization
    Gates removed:           73
    Rotations merged:        41

Hardware
    Target:                 <target>
    Calibration:            <timestamp>

Estimated execution
    Fidelity:               <estimate>
    Duration:               <estimate>

Verification
    Equivalence:            PASS
    Statistical checks:     PASS
```

Reports should eventually have both human-readable and machine-readable formats.

---

# 32. Explainability

A mature compiler should eventually explain individual decisions.

For example:

```text
SWAP inserted because logical qubits q4 and q9
were not adjacent on the selected physical topology.
```

Or:

```text
Route B selected.

Route A:
    estimated error = 0.032

Route B:
    estimated error = 0.018
```

Or:

```text
Rz(theta) and Rz(phi) merged because they commute.

Result:
    Rz(theta + phi)
```

This will be extremely important once the optimization system becomes sophisticated.

---

# 33. Backend Architecture

Backends should implement a stable target abstraction.

A backend should describe:

* supported gates
* native gates
* connectivity
* physical qubit count
* gate durations
* gate fidelity
* measurement behavior
* reset behavior
* calibration
* scheduling constraints

Potential backend families include:

* Sirraya QuTub
* superconducting systems
* trapped-ion systems
* neutral-atom systems
* custom research hardware
* simulator-only targets

The compiler core should not assume all quantum technologies behave alike.

---

# 34. Native Gate Model

The native gate set should not be permanently hard-coded into the entire compiler.

Instead, a target should expose something conceptually like:

```text
NativeGateSet
```

One target might provide:

```text
Rz
Ry
Rzz
```

Another might provide an entirely different native set.

The synthesis engine should query the target capabilities.

---

# 35. Approximate Synthesis

Approximation is useful but must be controlled.

Every approximate synthesis method should specify:

* target operation
* approximation algorithm
* tolerance
* error bound
* expected gate count
* verification method

The compiler must never silently replace exact synthesis with approximate synthesis.

---

# 36. Resource Estimation

The system should eventually report:

```text
Logical qubits
Physical qubits
Total gates
1Q gates
2Q gates
Measurements
Resets
Logical depth
Physical depth
SWAP count
Estimated duration
Estimated error
Estimated fidelity
```

This data should be available programmatically as well as through reports.

---

# 37. Compilation Profiles

The compiler should support explicit optimization profiles.

For example:

```text
GateCount
Depth
Fidelity
Latency
Balanced
Research
Debug
```

A profile can define:

* pass pipeline
* routing strategy
* cost model
* synthesis strategy
* verification level
* reporting level

This avoids forcing one definition of "best circuit" onto every user.

---

# 38. Determinism

Compilation should be deterministic by default.

If an algorithm uses randomness, the seed must be controllable.

A compilation should be reproducible from:

```text
Source circuit
Compiler version
Configuration
Backend
Calibration snapshot
Random seed
```

This becomes critical for debugging, benchmarking, and scientific research.

---

# 39. API Stability

The project should distinguish:

```text
Stable API
Internal API
Experimental API
Research API
```

Internal implementation details should not accidentally become public contracts.

A public API should remain stable even when internal compiler implementations evolve.

---

# 40. Architecture Decision Records

Major architectural decisions should be preserved in dedicated ADRs.

Examples:

```text
ADR-0001 — IR Design
ADR-0002 — Pass Manager
ADR-0003 — Routing Interface
ADR-0004 — Hardware Model
ADR-0005 — Fidelity Estimation
ADR-0006 — Parameter System
ADR-0007 — Verification Strategy
```

Each decision should document:

* Context
* Problem
* Decision
* Alternatives
* Reasoning
* Consequences
* Status

This is critical when engineering teams change.

---

# 41. Research Track

The project should clearly separate:

**Stable engineering**

from:

**Experimental research**

Research areas may include:

* new routing algorithms
* novel cost functions
* approximate synthesis
* calibration-adaptive compilation
* advanced noise-aware optimization
* new verification techniques
* new mathematical compilation techniques

Experimental code should not silently become production behavior.

---

# 42. Benchmarking

A permanent benchmark suite should measure:

* compilation time
* gate reduction
* depth reduction
* SWAP reduction
* estimated fidelity
* estimated runtime
* memory consumption
* verification cost

Benchmarks should cover:

* small circuits
* large circuits
* deep circuits
* wide circuits
* routing-heavy circuits
* noise-sensitive circuits
* parameterized circuits

An optimization should never be declared "better" without specifying the metric.

---

# 43. Performance Engineering

As circuits become larger, important performance areas will include:

* IR allocation
* cloning
* graph traversal
* dependency computation
* routing search
* symbolic simplification
* verification
* memory consumption

Performance work should be driven by profiling.

Correctness should not be sacrificed for premature optimization.

---

# 44. Security and Robustness

The parser and compiler should treat input as potentially untrusted.

Important areas include:

* malformed QASM
* unsupported operations
* resource exhaustion
* recursion limits
* numerical instability
* invalid qubit references
* invalid mappings
* dependency vulnerabilities

The compiler must never silently skip unsupported operations.

---

# 45. Technical Defensibility

The objective should **not** be to make QuTub large simply so that someone cannot copy it.

A 10,000-line codebase can still be copied.

The real moat should be:

```text
Deep compiler architecture
        +
Research algorithms
        +
Verification infrastructure
        +
Hardware models
        +
Calibration knowledge
        +
Benchmark corpus
        +
Engineering history
        +
Specialized optimization
        +
Execution integrations
```

This is much more difficult to reproduce.

The project should therefore grow because the problem itself demands sophisticated infrastructure.

---

# 46. What 10,000+ Lines Should Represent

A mature compiler can naturally exceed 10,000 lines through genuine capabilities such as:

```text
IR
+
Pass Manager
+
Analysis
+
Optimization
+
Placement
+
Routing
+
Synthesis
+
Backend abstraction
+
Calibration
+
Noise models
+
Scheduling
+
Verification
+
Reporting
+
Benchmarking
+
Symbolic mathematics
+
Hamiltonian support
```

The correct engineering question is:

> What capability does this subsystem provide?

If the answer is nothing meaningful, the code probably should not exist.

---

# 47. Long-Term Development Roadmap

## Phase 0 — Foundation

* stabilize current behavior
* document current architecture
* strengthen regression testing
* establish benchmark corpus
* establish ADRs
* establish versioning

**Exit condition:**
A new engineer can understand, build, test, and modify the current compiler without private knowledge.

---

## Phase 1 — Compiler Core

Build:

* stronger IR
* typed operations
* qubit abstractions
* parameter representation
* pass manager
* analysis interfaces
* compiler diagnostics

**Exit condition:**
The compilation pipeline is no longer hard-coded around individual passes.

---

## Phase 2 — Analysis and Optimization

Build:

* dependency DAG
* depth analysis
* critical path
* commutation
* identity registry
* advanced cancellation
* Clifford optimization
* cost model

**Exit condition:**
Optimization decisions are analysis-driven and measurable.

---

## Phase 3 — Placement and Routing

Build:

* topology abstraction
* interaction graph
* initial placement
* routing interface
* multiple routing strategies
* routing cost models

**Exit condition:**
Routing algorithms can be exchanged without redesigning the compiler.

---

## Phase 4 — Hardware Awareness

Build:

* hardware model
* calibration snapshots
* gate duration
* gate fidelity
* readout error
* physical connectivity
* target-specific constraints

**Exit condition:**
Compilation decisions can use actual target characteristics.

---

## Phase 5 — Noise-Aware Compilation

Build:

* error models
* error propagation
* error budgets
* noise-aware cost functions
* fidelity-aware routing
* noise-aware synthesis

**Exit condition:**
The compiler can intentionally trade circuit size against expected execution quality.

---

## Phase 6 — Scheduling

Build:

* ASAP scheduling
* ALAP scheduling
* duration-aware scheduling
* critical-path optimization
* resource constraints

**Exit condition:**
Compiled circuits have meaningful execution timing.

---

## Phase 7 — Verification

Build:

* unitary equivalence
* randomized equivalence
* property-based testing
* statistical measurement verification
* differential testing
* permanent regression corpus

**Exit condition:**
Every major transformation has a documented verification strategy.

---

## Phase 8 — Symbolic Compilation

Build:

* symbolic parameters
* expression trees
* symbolic simplification
* parameter-aware optimization
* parameterized verification

**Exit condition:**
Parameterized circuits can be optimized without requiring concrete parameter values.

---

## Phase 9 — Quantum Algorithm Infrastructure

Build:

* Pauli algebra
* Hamiltonians
* Pauli rotations
* Hamiltonian evolution
* Trotterization
* measurement grouping

**Exit condition:**
Higher-level quantum algorithm structures can be represented and compiled.

---

## Phase 10 — Advanced Research Compiler

Explore:

* advanced synthesis
* approximate synthesis
* advanced routing
* multi-objective optimization
* calibration-adaptive compilation
* automated optimization search
* advanced verification
* research-oriented compilation strategies

**Exit condition:**
QuTub becomes a platform for quantum compilation research rather than only a transpilation library.

---

# 48. Team Structure

As the system grows, teams may naturally divide into:

### Compiler Core

Responsible for:

* IR
* pass manager
* frontend
* compiler infrastructure

### Optimization

Responsible for:

* algebra
* commutation
* Clifford optimization
* symbolic optimization

### Architecture

Responsible for:

* placement
* topology
* routing

### Hardware

Responsible for:

* hardware models
* calibration
* noise

### Synthesis

Responsible for:

* gate decomposition
* Hamiltonian evolution
* approximate synthesis

### Verification

Responsible for:

* equivalence
* testing
* statistical validation

### Tooling

Responsible for:

* CLI
* reporting
* benchmarks
* documentation

One engineer may own multiple areas early in the project.

The architectural boundaries should still remain.

---

# 49. Ownership and Handover

No critical subsystem should depend on only one person's undocumented knowledge.

Each important subsystem should have:

* primary maintainer
* secondary maintainer
* documentation owner
* test owner

When an engineer changes teams or leaves, the handover should cover:

* implementation status
* open issues
* known bugs
* architectural decisions
* performance limitations
* research references
* benchmark results
* test gaps
* unfinished work
* recommended next steps

The goal is institutional memory.

---

# 50. Definition of Done

A feature is not complete simply because it compiles.

A mature definition of done is:

```text
Implementation
      +
Tests
      +
Documentation
      +
Diagnostics
      +
Benchmark
      +
Verification
      +
Known limitations
```

The depth of each requirement can vary according to feature size.

---

# 51. New Gate or Identity Checklist

Before introducing a new identity:

```text
Mathematical identity documented
Conditions documented
Exact/approximate status defined
Implementation added
Randomized verification added
Regression test added
Performance impact measured
Documentation updated
```

---

# 52. New Backend Checklist

Before adding a backend:

```text
Native gate set defined
Connectivity defined
Qubit model defined
Calibration representation defined
Gate durations defined where available
Measurement model defined
Routing constraints defined
Backend lowering implemented
Verification suite added
Example circuits added
Benchmarks collected
Documentation added
```

---

# 53. New Optimization Checklist

Before merging an optimization:

```text
Semantic preconditions identified
Mathematical basis documented
Correct compiler layer identified
Interactions with other passes understood
Numerical stability considered
Randomized verification added
Regression tests added
Benchmark measured
Cost-model impact documented
```

---

# 54. Failure Philosophy

Compiler failures must be explicit.

Prefer errors such as:

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

Never:

* silently remove a gate
* silently approximate
* silently change measurement semantics
* silently alter backend behavior
* silently fall back to an unexpected target

A compiler should fail loudly rather than produce a plausible but incorrect circuit.

---

# 55. Numerical Policy

Quantum compilation relies heavily on floating-point mathematics.

The project should centralize policies for:

* angle normalization
* near-zero comparisons
* matrix comparison
* fidelity comparison
* symbolic-to-numeric conversion
* approximation tolerance

Avoid scattering arbitrary constants throughout the system.

Instead, establish named numerical policies.

---

# 56. CLI Direction

A future command-line interface could conceptually support:

```text
qutub-transpile input.qasm
```

with options for:

* backend
* compilation profile
* optimization level
* output format
* report generation
* verification

Future capabilities could include:

```text
inspect
analyze
benchmark
verify
explain
```

The CLI should remain a consumer of the compiler library.

Compiler logic should not be trapped inside the CLI.

---

# 57. Compilation Artifact

A future compiled artifact should preserve:

```text
Source hash
Compiler version
Backend
Calibration snapshot
Compilation profile
Configuration
Logical circuit
Physical circuit
Qubit mapping
Scheduling
Statistics
Verification status
Error estimate
```

This provides reproducibility and makes compiled outputs scientifically useful.

---

# 58. End-to-End Target Architecture

The mature system should conceptually look like:

```text
                 QUANTUM PROGRAM
                        |
                        v
                    FRONTEND
                        |
                        v
                CANONICAL IR
                        |
                        v
                    ANALYSIS
            /           |           \
           /            |            \
 Dependencies      Commutation    Interaction
    |                   |             Graph
    +-------------------+-------------+
                        |
                        v
               SOURCE OPTIMIZATION
                        |
                        v
                    PLACEMENT
                        |
                        v
                     ROUTING
                        |
                        v
              TECHNOLOGY-AWARE
                   SYNTHESIS
                        |
                        v
                 BACKEND LOWERING
                        |
                        v
              NATIVE OPTIMIZATION
                        |
                        v
            CALIBRATION-AWARE COST
                    EVALUATION
                        |
                        v
                   SCHEDULING
                        |
                        v
                  VERIFICATION
                  /          \
                 /            \
                v              v
       VALIDATED CIRCUIT    REPORT
                |
                v
        SIRRAYA QUTUB /
        TARGET BACKEND
```

---

# 59. Most Important Architectural Insight

The compiler must eventually optimize:

**Mathematically valid circuit**

into:

**Physically appropriate circuit**

Those are different problems.

A mathematically optimal decomposition can be physically poor.

A gate-count-optimal circuit can have worse fidelity.

A minimum-SWAP route can have worse calibration characteristics.

A shorter circuit can have a worse critical path.

A deeper circuit can sometimes be preferable if it avoids particularly poor hardware interactions.

Therefore QuTub should eventually reason simultaneously about:

```text
Semantics
Topology
Calibration
Noise
Time
Resources
Verification
```

That is the core technical identity of the system.

---

# 60. What QuTub Should Eventually Become

The long-term goal is not:

```text
QASM → gates
```

It is:

```text
Quantum Program
       |
       v
Semantic Understanding
       |
       v
Compiler Analysis
       |
       v
Mathematical Optimization
       |
       v
Physical Mapping
       |
       v
Hardware-Aware Routing
       |
       v
Technology-Aware Synthesis
       |
       v
Calibration-Aware Optimization
       |
       v
Execution Scheduling
       |
       v
Independent Verification
       |
       v
Validated Execution Artifact
```

In one sentence:

> **Sirraya QuTub Transpiler should become a quantum compiler that understands not only what a circuit means, but what it will cost to execute that circuit on a real target system.**

---

# 61. Long-Term Success Criteria

QuTub should eventually satisfy the following:

```text
Multiple frontends → one IR

Multiple backends → one compiler architecture

Passes → composable

Routing algorithms → replaceable

Cost models → replaceable

Calibration snapshots → versioned

Hardware-aware compilation → supported

Noise-aware optimization → supported

Parameterized circuits → supported

Symbolic optimization → supported

Verification → systematic

Reports → reproducible

Benchmarks → continuously maintained

New engineers → able to understand architecture

Research experiments → isolated from stable production
```

---

# 62. Engineering Culture

The team should prefer:

```text
Small interfaces
Explicit assumptions
Mathematical documentation
Reproducible experiments
Strong tests
Measured performance
Clear ownership
Reviewable changes
```

over:

```text
Clever abstractions
Undocumented tricks
Large rewrites
Premature optimization
Opaque magic
Unverified benchmark claims
```

Advanced algorithms are acceptable.

Unnecessary complexity is not.

The system should remain understandable even when its underlying mathematics becomes sophisticated.

---

# 63. Guidance for Future Engineers

Before modifying an unfamiliar subsystem, answer:

1. What semantic guarantee does this subsystem provide?
2. What assumptions does it make?
3. Which IR level does it operate on?
4. Which analyses does it consume?
5. Which analyses does it invalidate?
6. Which mathematical identities does it rely on?
7. How is correctness verified?
8. What hardware assumptions exist?
9. What cost function is being optimized?
10. What happens when the subsystem fails?
11. Is its behavior deterministic?
12. Can its decisions be explained?
13. What benchmark demonstrates the change?
14. Which ADR describes the architecture?
15. What future feature could this interface accidentally prevent?

If these questions cannot be answered, the subsystem needs better documentation before it needs more features.

---

# 64. Recommended Implementation Order

The team should **not attempt to build the entire architecture simultaneously**.

The recommended order is:

```text
1. Freeze and document current behavior
2. Strengthen IR
3. Introduce PassManager
4. Introduce analysis interfaces
5. Build dependency DAG
6. Improve optimization
7. Introduce topology abstraction
8. Improve placement
9. Introduce routing strategies
10. Introduce HardwareModel
11. Introduce calibration snapshots
12. Introduce CostModel
13. Add noise-aware routing
14. Add scheduling
15. Strengthen verification
16. Add reporting
17. Add symbolic parameters
18. Add Pauli algebra
19. Add Hamiltonians
20. Add advanced synthesis
21. Expand backend ecosystem
22. Build research optimization framework
```

Each stage should preserve the correctness of the preceding stage.

---

# 65. Final Engineering Principle

QuTub should not become large merely for the sake of becoming large.

The goal is not:

> "Make the repository 10,000 lines."

The goal is:

> **Build a system whose depth naturally results from solving the real problems involved in quantum compilation.**

The durable technical moat should therefore be:

```text
Architecture
+
Algorithms
+
Verification
+
Hardware knowledge
+
Calibration data
+
Benchmarks
+
Documentation
+
Accumulated engineering decisions
```

That is significantly more defensible than simply having a large codebase.

The objective is not to make QuTub difficult for engineers to understand.

The objective is to make QuTub **deep enough that reproducing the complete system requires substantial quantum, mathematical, compiler, hardware, and software-engineering expertise, along with years of accumulated validation and research.**

---

# 66. Document Maintenance

This reference should be updated whenever:

* a major architectural boundary changes
* a new compiler layer is introduced
* the backend abstraction changes
* a major optimization strategy is adopted
* a research direction becomes production-supported
* an ADR supersedes an existing decision
* ownership of a critical subsystem changes

Recommended review cadence:

```text
Minor review:              Quarterly
Architecture review:       Every 6 months
Major roadmap review:      Annually
```

When this document conflicts with an approved ADR, the ADR should be treated as authoritative for that specific architectural question. The main engineering reference should then be updated to reflect the decision.

---


