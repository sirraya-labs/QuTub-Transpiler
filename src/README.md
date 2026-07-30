# Bell-state end-to-end: crate -> real IBM QASM -> hardware/simulator

## What's here

- **`native.rs`** (append the tail of this file onto your existing `src/native.rs`,
  or just drop this whole file in) — adds one small crate-visible helper,
  `approx_eq_up_to_global_phase`, reused by the new module's tests. Nothing
  else in the file changed.
- **`ibm_export.rs`** — new module, drop into `src/ibm_export.rs`. Expands
  `BackendGate::Rot` into the real `Rz`/`SX`/`X` sequence IBM hardware
  actually runs (your existing model treats `Rot` as a free-angle `Rx`,
  which is a fine internal simplification but not something IBM's pulses
  can do directly), and exports real OpenQASM 2.0 using IBM's own basis
  gate names.
- **`lib.rs`** — updated with `pub mod ibm_export;` and the corresponding
  re-exports. Everything else is unchanged from your version.
- **`bell_state_end_to_end.rs`** — drop into `examples/bell_state_end_to_end.rs`.
  Runs a Bell state through the real pipeline, writes `bell.qasm`, and
  produces a local-simulator reference histogram (`bell_reference_counts.json`)
  to compare a real hardware run against.
- **`submit_ibm.py`** — the actual submission bridge, since there's no
  Rust SDK for IBM Quantum Platform. Works against a local Qiskit
  simulator immediately (no account needed), and against real hardware
  once you have credentials.

## Important caveat

I could not compile or run any of this against your actual crate —
`sirraya_qutub` itself wasn't uploaded (only referenced from doc
comments), and this sandbox has no Rust toolchain and no network
access to install one. What I *did* verify, independently, before
writing any of it:

- **The core math.** The `Rot` -> `Rz`/`SX` identity
  (`Rx(theta) == Rz(pi/2).SX.Rz(theta+pi).SX.Rz(pi/2)`, up to global
  phase) was derived symbolically with sympy and checked numerically
  against a dozen angles, including 0, pi, negative angles, and
  angles beyond 2*pi — see the transcript above. That's the one part
  of this that would have been expensive to get subtly wrong (it would
  silently produce the wrong quantum state on real hardware while
  still looking plausible), so it's also now baked in as a real
  `#[test]` in `ibm_export.rs`
  (`expand_rot_matches_rx_for_a_spread_of_angles`), reusing your
  already-tested `m_rz`/`m_rx` matrix builders from `native.rs`.
- **The wiring.** `BackendCircuit`'s fields are all `pub`, `Backend`
  derives `PartialEq`, `BackendGate` derives `Copy` — I checked each of
  these against your actual uploaded source rather than assuming.

What I couldn't verify: that the whole thing actually compiles.
Run `cargo test` after dropping these in — if anything doesn't line
up (an import path, a re-export I guessed wrong), it should be a
small, obvious fix rather than a logic problem.

## Steps to actually get one circuit through

1. Drop in the four Rust files above, run `cargo test` — this alone
   validates the new `Rot` decomposition against your existing
   matrix algebra, no IBM account needed yet.
2. `cargo run --example bell_state_end_to_end` — writes `bell.qasm`
   (real IBM-basis QASM) and `bell_reference_counts.json` (your
   simulator's own Bell-state histogram over 4096 shots).
3. Sanity-check the QASM parses and runs locally, still no IBM
   account required:
   ```
   pip install qiskit qiskit-aer --break-system-packages
   python3 submit_ibm.py --qasm bell.qasm --shots 4096 \
       --compare bell_reference_counts.json
   ```
   The reported total variation distance should be small (both are
   noiseless simulators of the same circuit) — this step is really
   just confirming the QASM export is well-formed and loadable.
4. Get IBM Quantum Platform access (quantum.ibm.com), generate an API
   token, and find your instance CRN.
5. Run for real:
   ```
   pip install qiskit-ibm-runtime --break-system-packages
   export IBM_QUANTUM_TOKEN=...
   export IBM_QUANTUM_INSTANCE=...
   python3 submit_ibm.py --qasm bell.qasm --shots 4096 --real \
       --backend <a real backend name from your account> \
       --compare bell_reference_counts.json
   ```
   Now the total variation distance is the real number you're after:
   how far a real device's Bell-state output drifts from ideal. A
   healthy modern IBM device should land clearly closer to 0 than to
   1 — if it doesn't, that's the first real signal to dig into
   (routing, an inverted `Cx` direction relative to the device's
   actual connectivity, etc.) rather than assuming the model's wrong.

## Known simplification still in the model

`ibm_export.rs`'s doc comment flags this explicitly: this bridges
`BackendGate::Rot`/`Cx` to real pulses, but it does **not** yet pull
the target device's *actual* coupling map or basis gate (some IBM
processors use `ECR` rather than `CX` as their native two-qubit gate)
from IBM's API — `backend::lower` still routes against the generic
`heavy_hex_for` topology generator in `coupling.rs`. For a single
small circuit like this Bell pair that's unlikely to matter (it'll
either route trivially or fail loudly), but it's the next real gap
once this round-trip is working.
