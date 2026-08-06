//! Lowers a [`Circuit`] to a specific backend's native gate set, instead
//! of only the trapped-ion-style `{Rz, Ry, Rzz}` target in [`crate::native`].
//!
//! # Backends implemented
//! Each backend is an implementation of [`BackendSpec`]
//! in its own file, referenced by one [`Backend`] constant -- see
//! `backend/spec.rs`'s module doc for why this is a trait rather than a
//! closed `match`-per-variant enum, and for what a new backend needs to
//! implement to plug in here.
//! - [`Backend::TrappedIon`] (`backend/trapped_ion.rs`) -- `{Rz, Ry, Rzz}`.
//!   Its native gate set already *is* [`crate::native::decompose`]'s own
//!   canonical output, so lowering is just relabeling (see
//!   `BackendSpec::is_native_decompose_target`).
//! - [`Backend::IbmQ`] (`backend/ibmq.rs`) -- `{Rz, Rx, Cx}`, modeling
//!   IBM's superconducting basis (virtual-Z framing + a native two-qubit
//!   `CNOT`).
//! - [`Backend::Rigetti`] (`backend/rigetti.rs`) -- `{Rz, Rx, Cz}`,
//!   modeling Rigetti's superconducting basis (`CZ`-native rather than
//!   `CNOT`-native).
//! - [`Backend::Google`] (`backend/google.rs`) -- `{Rz, Rx, Cz}`,
//!   modeling Google's Willow processor in its CZ-tuned configuration.
//!   Added after the three above, as the actual test of this module's
//!   extension story: it needed a new file and one new `Backend::`
//!   constant, nothing else here changed, and it turned out to need
//!   *zero* new gate-identity derivations -- see `backend/google.rs`'s
//!   own doc comment for why a fourth, independently-sourced backend
//!   landing on the exact same `push_two_qubit_zz` identity `Rigetti`
//!   already uses isn't a coincidence worth suppressing.
//!
//! Two circuit identities, generic across every non-`TrappedIon`
//! backend, do the actual re-expansion work in *this* file (each
//! backend's own file supplies only the piece that's genuinely
//! backend-specific -- see `push_two_qubit_zz`'s doc comment):
//! 1. `Ry(theta) == Rx(-pi/2) . Rz(theta) . Rx(pi/2)` (see [`push_ry`])
//!    -- reused directly from the `RYY` decomposition in `native.rs`
//!    (same Y = Rx(-pi/2).Z.Rx(pi/2) fact, exponentiated), so any
//!    `Rx`-axis backend's single-qubit gates reuse the *same* ZYZ
//!    synthesis as the trapped-ion target and just re-express the
//!    resulting `Ry` calls.
//! 2. `Rzz(a, b, theta) == Cx(a, b) . Rz(b, theta) . Cx(a, b)` -- the
//!    reason `Cx` is exactly as cheap on `IbmQ` as `Rzz` is on
//!    `TrappedIon` (one native two-qubit gate). `IbmQ` uses this
//!    directly; `Rigetti` and `Google` (both `Cz`-native, no native
//!    `Cx`) each use a shortened variant of it -- see
//!    `backend/rigetti.rs`'s own doc comment for the third identity
//!    that gets it there in 2 `H`'s instead of 4.
//!
//! # What's not here: Pasqal (neutral atoms) or a real photonic backend
//! Neutral-atom platforms (Pasqal, and analog/digital Rydberg-blockade
//! devices generally) and photonic platforms aren't a [`BackendSpec`]
//! on purpose -- unlike `Google` above, adding either isn't blocked by
//! "write one file and register one constant," because neither one's
//! physics fits what this trait is a contract for in the first place.
//! Pasqal's native "two-qubit gate" is a blockade interaction between
//! whichever atoms are currently within blockade radius of each other
//! in a *movable, laser-tweezer-defined* 2D/3D layout -- so "compiling
//! to Pasqal's native gates" is inseparable from *placing* the atoms and
//! routing which pairs are ever simultaneously in blockade range, which
//! is a materially different problem from "express this unitary in
//! terms of a fixed two-qubit gate" (the problem this module and
//! `native.rs` solve, and what `BackendSpec::push_two_qubit_zz` assumes
//! every implementor is doing -- see `backend/spec.rs`'s module doc).
//! Pasqal does also expose a "digital" mode with a fixed local
//! `CZ`-like gate (making it superficially similar to `Rigetti`/`Google`
//! here), but modeling it correctly still needs blockade-radius/layout
//! constraints this crate doesn't have. A photonic backend's native
//! gates (beamsplitters/phase shifters on modes, typically
//! probabilistic two-qubit interaction) don't have a `Rot`/`Rzz` shape
//! at all -- see `backend/spec.rs`'s module doc for the fuller version
//! of this argument.
//!
//! A photonic backend runs into the same wall from a different
//! direction: linear-optical qubits (dual-rail, or continuous-variable
//! encodings) don't have a `BackendGate::Rot`/`Rzz`-shaped native gate
//! set at all -- their primitives are beamsplitters and phase shifters
//! acting on modes, not qubit-indexed rotations, and (for dual-rail,
//! KLM-style) two-qubit gates are probabilistic/measurement-induced
//! rather than deterministic unitaries. `BackendSpec` as written can't
//! express that honestly. Shipping a `Backend::Photonic` that reused
//! `Rigetti`'s or `IbmQ`'s gate identities under a different name would
//! be presenting untested, physically wrong gate identities as
//! equivalent to the three backends above, which were each tested
//! against the real simulator the way `native.rs` was. A real photonic
//! backend needs its own gate representation (likely a new
//! `BackendGate`-like enum, not a `BackendSpec` impl of this one) --
//! left as a follow-on, same as Pasqal.

use crate::ir::{Circuit, Gate};
use std::f64::consts::FRAC_PI_2;

mod google;
mod ibmq;
mod rigetti;
mod spec;
mod trapped_ion;

pub use spec::{Backend, BackendSpec, RotAxis};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackendGate {
    Rz(usize, f64),
    /// The backend's native continuously-variable single-qubit
    /// rotation: `Ry` for `TrappedIon`, `Rx` for `IbmQ`/`Rigetti`. (For
    /// `IbmQ`/`Rigetti` this is a modeling simplification of a fixed
    /// `SX`/`SX-dagger` pulse as a continuously-parameterized rotation
    /// about the same axis -- see this module's doc comment.)
    Rot(usize, f64),
    /// `IbmQ`'s native two-qubit gate.
    Cx(usize, usize),
    /// `Rigetti`'s native two-qubit gate.
    Cz(usize, usize),
    /// `TrappedIon`'s native two-qubit gate.
    Rzz(usize, usize, f64),
    /// Passed through unchanged from `ir::Gate::Measure` on every
    /// backend -- not a unitary rewrite target, so no lowering
    /// identity in this module ever targets it.
    Measure(usize, usize),
}

#[derive(Debug, Clone)]
pub struct BackendCircuit {
    pub backend: Backend,
    pub num_qubits: usize,
    pub num_clbits: usize,
    pub gates: Vec<BackendGate>,
}

impl BackendCircuit {
    fn new(backend: Backend, num_qubits: usize) -> Self {
        Self {
            backend,
            num_qubits,
            num_clbits: 0,
            gates: Vec::new(),
        }
    }
    pub(crate) fn push(&mut self, g: BackendGate) {
        self.gates.push(g);
    }

    /// (single_qubit_gate_count, two_qubit_gate_count) -- the two
    /// numbers a per-backend fidelity budget needs. `Measure` is
    /// excluded from both, for the same reason `NativeCircuit::gate_counts`
    /// excludes it: it isn't a unitary gate, so a depolarizing-error
    /// budget shouldn't price it as one.
    pub fn gate_counts(&self) -> (usize, usize) {
        let mut single = 0;
        let mut two = 0;
        for g in &self.gates {
            match g {
                BackendGate::Rz(..) | BackendGate::Rot(..) => single += 1,
                BackendGate::Cx(..) | BackendGate::Cz(..) | BackendGate::Rzz(..) => two += 1,
                BackendGate::Measure(..) => {}
            }
        }
        (single, two)
    }
}

pub(crate) const EPS: f64 = 1e-9;

/// Lowers a source-level circuit straight to `backend`'s native gate set.
///
/// Routes against `backend`'s [`Backend::coupling_map`] first (see
/// `coupling.rs`/`route.rs`) -- a no-op for `TrappedIon`, which has
/// none -- so every two-qubit gate below is already guaranteed to sit
/// on physical qubits the backend can actually apply a native two-qubit
/// gate to directly. Everything from here down is unchanged from
/// before routing existed: it only ever reads `circuit`, so it doesn't
/// know or care whether `circuit` is the caller's original or an
/// already-routed one.
///
/// Uses [`crate::route::route_best`] rather than calling
/// [`crate::route::route_lookahead`] directly: `route_best` runs
/// [`crate::route::route`], `route_lookahead`, and
/// [`crate::route::route_sabre`] and keeps whichever used fewest SWAPs
/// (see that function's own doc comment for why no single one of the
/// three is a strict improvement on the other two in every case). All
/// three are exactly semantics-preserving (see `route.rs`'s own
/// `assert_lookahead_routing_preserves_action`/
/// `assert_sabre_routing_preserves_action` coverage), so picking
/// between them by SWAP count alone never risks correctness, only
/// performance -- and every SWAP saved is 3 fewer native two-qubit
/// gates once lowered below (Rzz/Cx/Cz).
pub fn lower(circuit: &Circuit, backend: Backend) -> BackendCircuit {
    lower_with_coupling(circuit, backend, backend.coupling_map(circuit.num_qubits).as_ref())
}

/// As [`lower`], but routes against `coupling` (if given) instead of
/// `backend`'s own default topology (`Backend::coupling_map` --
/// currently always one of `coupling.rs`'s *synthetic* generators,
/// `heavy_hex_for`/`square_grid_for`, not any specific real chip's
/// actual wiring). `lower` itself is now just this function called
/// with `backend.coupling_map(circuit.num_qubits)`.
///
/// This exists because the synthetic default is a real correctness
/// risk once a circuit is headed for a real device rather than a
/// simulator: `ibm_export.rs`'s own module doc already flags that this
/// crate does no live device coupling-map query, so a circuit routed
/// against the synthetic map has no guarantee its two-qubit gates land
/// on pairs that are actually coupled on the specific chip a job gets
/// submitted to (disabled qubits, chip-specific layout -- see
/// [`crate::coupling::CouplingMap::from_edges`]'s own doc comment).
/// `submit_ibm.py --dump-coupling-map` queries a real backend's edge
/// list via Qiskit; build a [`CouplingMap`] from that with `from_edges`
/// and pass it here instead of relying on `lower`'s synthetic default.
///
/// `coupling == None` means route against no topology at all (every
/// pair adjacent) -- the same behavior `lower` already has for
/// `TrappedIon`, for which `Backend::coupling_map` always returns
/// `None`. Passing `None` for `IbmQ`/`Rigetti` is a real (if unusual)
/// choice too, e.g. an all-to-all simulator target; it is never
/// implied by "I don't have a real coupling map yet" -- an absent map
/// silently skips routing rather than erroring, so a caller with no
/// real topology to hand should keep using `lower`'s synthetic default
/// rather than pass `None` here by omission.
pub fn lower_with_coupling(
    circuit: &Circuit,
    backend: Backend,
    coupling: Option<&crate::coupling::CouplingMap>,
) -> BackendCircuit {
    let routed_storage;
    let circuit: &Circuit = match coupling {
        Some(coupling) => {
            routed_storage = crate::route::route_best(circuit, coupling);
            &routed_storage
        }
        None => circuit,
    };

    // Reuse the same Rz/Ry/Rzz canonical form for every backend
    // (native.rs), then hand each gate to `backend`'s own
    // `BackendSpec` to re-express in its native gate set.
    let mut bc = BackendCircuit::new(backend, circuit.num_qubits);
    bc.num_clbits = circuit.num_clbits;

    if backend.is_native_decompose_target() {
        // This backend's native gate set already *is*
        // native::decompose's canonical output (true only for
        // TrappedIon today) -- nothing to re-express, just relabel.
        let native = crate::native::decompose(circuit);
        for g in &native.gates {
            bc.push(match *g {
                crate::native::NativeGate::Rz(q, a) => BackendGate::Rz(q, a),
                crate::native::NativeGate::Ry(q, a) => BackendGate::Rot(q, a),
                crate::native::NativeGate::Rzz(a, b, t) => BackendGate::Rzz(a, b, t),
                crate::native::NativeGate::Measure(q, c) => BackendGate::Measure(q, c),
            });
        }
        optimize(&mut bc);
    } else {
        let axis = backend.rot_axis();
        // Checked once per circuit, not per gate -- see
        // `BackendSpec::has_native_cx`'s doc comment on why this must
        // be a backend-wide constant.
        let native_cx = backend.has_native_cx();
        // Unlike the TrappedIon branch above, this walks `circuit`'s
        // own source gates one at a time (via native.rs's per-gate
        // `decompose_gate`, exposed for exactly this) instead of
        // calling `native::decompose` once over the whole circuit
        // up front. That lets a `Gate::Cx` (or a routing-inserted
        // `Gate::Swap`, which is `Cx(a,b).Cx(b,a).Cx(a,b)` at the IR
        // level -- see `route.rs`) take `push_native_cx`'s cheaper
        // path *before* `decompose_gate`'s own generic `H . Rzz . H`
        // expansion of `Cx` ever runs, for any backend that opted in.
        // Every other gate kind still goes through exactly the same
        // decompose-then-re-express pipeline as before; this is a
        // strictly additive fast path, not a rewrite of the general
        // one.
        for gate in &circuit.gates {
            match *gate {
                Gate::Cx(control, target) if native_cx => {
                    backend.push_native_cx(&mut bc, control, target);
                }
                Gate::Swap(a, b) if native_cx => {
                    backend.push_native_cx(&mut bc, a, b);
                    backend.push_native_cx(&mut bc, b, a);
                    backend.push_native_cx(&mut bc, a, b);
                }
                _ => {
                    let mut nc = crate::native::NativeCircuit::new(circuit.num_qubits);
                    crate::native::decompose_gate(&mut nc, gate);
                    for g in &nc.gates {
                        match *g {
                            crate::native::NativeGate::Rz(q, a) => bc.push(BackendGate::Rz(q, a)),
                            crate::native::NativeGate::Ry(q, a) => push_ry(&mut bc, axis, q, a),
                            crate::native::NativeGate::Rzz(a, b, t) => {
                                backend.push_two_qubit_zz(&mut bc, a, b, t)
                            }
                            crate::native::NativeGate::Measure(q, c) => {
                                bc.push(BackendGate::Measure(q, c))
                            }
                        }
                    }
                }
            }
        }
        // The per-gate ZYZ re-expansion above (especially `H` inside a
        // `push_two_qubit_zz` like Rigetti's, and every `Ry` via
        // `push_ry`) is emitted independently of its neighbors, so
        // plenty of adjacent/commuting single-qubit rotations are left
        // unmerged -- and worse, `optimize`'s `Rot` fusion only ever
        // merges two *literally adjacent* `Rot`s; a run like
        // `Rot(pi/2).Rz(theta).Rot(-pi/2)` (one `push_ry` Rx-axis
        // block) sitting next to another such block from a neighboring
        // source gate is really one single-qubit unitary wearing up to
        // 6+ gates, and no adjacent-pair rule ever collapses that.
        // `resynthesize` (see its doc comment) closes that gap by
        // collapsing the whole run algebraically instead of
        // pattern-matching adjacent pairs; running it back-to-back
        // with `optimize` to a fixed point lets a two-qubit
        // cancellation freed up by one pass expose a longer
        // single-qubit run for the other, and vice versa.
        loop {
            let before = bc.gates.len();
            resynthesize(&mut bc);
            optimize(&mut bc);
            if bc.gates.len() == before {
                break;
            }
        }
    }
    bc
}

/// `Ry(theta) == Rx(-pi/2) . Rz(theta) . Rx(pi/2)` (apply `Rx(pi/2)`
/// first, then `Rz(theta)`, then `Rx(-pi/2)` last). See this module's
/// doc comment, identity 1.
fn push_ry_via_rx(bc: &mut BackendCircuit, q: usize, theta: f64) {
    bc.push(BackendGate::Rot(q, FRAC_PI_2));
    bc.push(BackendGate::Rz(q, theta));
    bc.push(BackendGate::Rot(q, -FRAC_PI_2));
}

/// Pushes a `Ry(q, theta)` in terms of whichever axis `axis` is this
/// backend's native `Rot`: emitted directly for `RotAxis::Ry`
/// (`TrappedIon`), or via [`push_ry_via_rx`]'s identity for
/// `RotAxis::Rx` (every other backend so far). Shared by [`lower`] (for
/// every source `Ry`) and [`push_h`] (which needs a `Ry` mid-identity
/// regardless of which axis it's re-expressing `H` for).
pub(crate) fn push_ry(bc: &mut BackendCircuit, axis: RotAxis, q: usize, theta: f64) {
    match axis {
        RotAxis::Ry => bc.push(BackendGate::Rot(q, theta)),
        RotAxis::Rx => push_ry_via_rx(bc, q, theta),
    }
}

/// Lowers a single `H(q)` by re-running it through the *same*
/// `native::decompose` + [`push_ry`] path every other single-qubit gate
/// takes here (rather than hand-deriving `H`'s specific `Rz`/`Ry`
/// angles a second time and risking a fresh sign error the way the
/// first version of `native.rs`'s ZYZ synthesis did). `axis` is the
/// caller's own backend's native rotation axis -- see each
/// `push_two_qubit_zz` implementation that needs an `H` (e.g.
/// `backend/rigetti.rs`) for why.
pub(crate) fn push_h(bc: &mut BackendCircuit, axis: RotAxis, q: usize) {
    let mut h_circuit = Circuit::new(q + 1);
    h_circuit.push(Gate::H(q));
    let canonical = crate::native::decompose(&h_circuit);
    for g in &canonical.gates {
        match *g {
            crate::native::NativeGate::Rz(qq, a) => bc.push(BackendGate::Rz(qq, a)),
            crate::native::NativeGate::Ry(qq, a) => push_ry(bc, axis, qq, a),
            crate::native::NativeGate::Rzz(..) => unreachable!("H never decomposes to Rzz"),
            crate::native::NativeGate::Measure(..) => {
                unreachable!("H never decomposes to Measure")
            }
        }
    }
}

/// Collapses every maximal run of single-qubit gates (`Rz`/`Rot`) on a
/// wire -- everything between one two-qubit gate touching that wire
/// and the next -- into a single canonical `Rz . Rot . Rz` triple (or
/// fewer gates, dropping any angle that's ~0), regardless of how many
/// gates the run started as.
///
/// This is a strictly stronger version of what `optimize`'s `Rot`
/// fusion already does for *literally adjacent* same-axis gates: here
/// the whole run's product matrix is accumulated (via
/// `crate::native`'s `Mat2`/`matmul`, the same algebra
/// `tests/decompositions.rs` already validates against the real
/// simulator) and re-synthesized from scratch, so gates separated by
/// real intervening `Rz`s -- which `optimize` correctly refuses to
/// merge across, since an `Rz` genuinely sitting between two `Rot`s is
/// a real event -- still collapse together here, because collapsing
/// the *whole run* is exact regardless of what's inside it.
///
/// For `IbmQ`/`Rigetti` (native axis `Rx`), the triple is derived from
/// `crate::native::zyz_decompose`'s `(delta, gamma, beta)` via the
/// exact identity `Ry(t) == Rz(pi/2) . Rx(t) . Rz(-pi/2)` (conjugating
/// `Ry` into `Rx` by the same 90-degree-about-Z rotation that maps the
/// X axis to the Y axis): substituting it into
/// `Rz(beta).Ry(gamma).Rz(delta) == m` and folding the two adjacent
/// `Rz`'s into their neighbors gives
/// `Rz(beta + pi/2) . Rx(gamma) . Rz(delta - pi/2) == m`. For
/// `TrappedIon` (native axis `Ry`), `zyz_decompose`'s own triple is
/// used directly with no shift.
pub fn resynthesize(bc: &mut BackendCircuit) {
    use crate::native::{m_identity, m_rx, m_ry, m_rz, matmul, zyz_decompose, Mat2};
    use std::collections::HashMap;

    let axis = bc.backend.rot_axis();

    fn single_qubit_matrix(axis: RotAxis, g: BackendGate) -> Option<(usize, Mat2)> {
        match g {
            BackendGate::Rz(q, a) => Some((q, m_rz(a))),
            BackendGate::Rot(q, a) => {
                let m = match axis {
                    RotAxis::Ry => m_ry(a),
                    RotAxis::Rx => m_rx(a),
                };
                Some((q, m))
            }
            _ => None,
        }
    }

    fn emit_synth(out: &mut Vec<BackendGate>, q: usize, m: Mat2, axis: RotAxis) {
        let (delta, gamma, beta) = zyz_decompose(m);
        let (first_z, mid, last_z) = match axis {
            RotAxis::Ry => (delta, gamma, beta),
            RotAxis::Rx => (delta - FRAC_PI_2, gamma, beta + FRAC_PI_2),
        };
        if !is_identity_angle(first_z) {
            out.push(BackendGate::Rz(q, wrap_angle(first_z)));
        }
        if mid.abs() > EPS {
            out.push(BackendGate::Rot(q, mid));
        }
        if !is_identity_angle(last_z) {
            out.push(BackendGate::Rz(q, wrap_angle(last_z)));
        }
    }

    fn flush(q: usize, acc: &mut HashMap<usize, Mat2>, out: &mut Vec<BackendGate>, axis: RotAxis) {
        if let Some(m) = acc.remove(&q) {
            emit_synth(out, q, m, axis);
        }
    }

    let mut acc: HashMap<usize, Mat2> = HashMap::new();
    let mut out: Vec<BackendGate> = Vec::with_capacity(bc.gates.len());

    for g in bc.gates.drain(..) {
        if let Some((q, m)) = single_qubit_matrix(axis, g) {
            let entry = acc.entry(q).or_insert_with(m_identity);
            *entry = matmul(m, *entry);
            continue;
        }
        match g {
            BackendGate::Cx(a, b) | BackendGate::Cz(a, b) => {
                flush(a, &mut acc, &mut out, axis);
                flush(b, &mut acc, &mut out, axis);
                out.push(g);
            }
            BackendGate::Rzz(a, b, _) => {
                flush(a, &mut acc, &mut out, axis);
                flush(b, &mut acc, &mut out, axis);
                out.push(g);
            }
            BackendGate::Measure(q, _) => {
                // A real event on wire q: any pending single-qubit
                // rotation accumulated for it must be emitted first,
                // the same as for a two-qubit gate touching q.
                flush(q, &mut acc, &mut out, axis);
                out.push(g);
            }
            BackendGate::Rz(..) | BackendGate::Rot(..) => unreachable!("handled above"),
        }
    }
    for q in acc.keys().copied().collect::<Vec<_>>() {
        flush(q, &mut acc, &mut out, axis);
    }
    bc.gates = out;
}

/// Wraps `theta` into `(-PI, PI]`, so a rotation that's an identity
/// mod `2*PI` is recognized as such regardless of which multiple of
/// `2*PI` it happened to accumulate as (e.g. two merged `Rz`'s summing
/// to `2*PI` are the identity, not "a big rotation").
fn wrap_angle(theta: f64) -> f64 {
    use std::f64::consts::PI;
    let mut t = theta % std::f64::consts::TAU;
    if t > PI {
        t -= std::f64::consts::TAU;
    } else if t <= -PI {
        t += std::f64::consts::TAU;
    }
    t
}

fn is_identity_angle(theta: f64) -> bool {
    wrap_angle(theta).abs() < EPS
}

/// Peephole-optimizes an already-lowered [`BackendCircuit`] in place.
/// Every gate emitted by [`lower`] comes from an *independent* ZYZ
/// re-expansion (each `Ry`/`H` re-derives its own `Rz`/`Rot` triple via
/// [`push_ry`] / [`push_h`]), so adjacent single-qubit rotations
/// on the same wire are frequently left unmerged, `Rz` -- being
/// diagonal -- is left un-commuted through neighboring two-qubit gates
/// it could otherwise pass straight through, and repeated two-qubit
/// gates on the same qubit pair (e.g. from adjacent `Rzz`'s in the
/// source circuit) are left un-cancelled/un-fused.
///
/// This pass does three things, all exact identities (no
/// approximation, no change to the implemented unitary mod global
/// phase):
///
/// 1. **Adjacent `Rot` fusion.** Two `Rot(q, a)` gates on the same
///    qubit with nothing else on that qubit between them collapse to
///    one `Rot(q, a+b)`.
/// 2. **`Rz` commutation + fusion.** `Rz(q, theta)` is diagonal, so it
///    commutes exactly with: any gate on a different qubit, `Cz(a,b)`
///    and `Rzz(a,b,t)` on *either* wire (both fully diagonal), and
///    `Cx(a,b)` on the control wire `a` (target-side transformation by
///    `X` doesn't touch the control's diagonal structure). It does
///    *not* commute with `Rot` on the same qubit, or with `Cx`'s target
///    wire `b`. Each `Rz` is therefore held as a "pending" rotation per
///    qubit and floated forward through anything it commutes with,
///    merging with any other pending `Rz` on that qubit it meets along
///    the way, and is only emitted (flushed) right before the first
///    gate on that qubit it *doesn't* commute with.
/// 3. **Same-pair two-qubit cancellation/fusion.** Two adjacent
///    `Cz(a,b)` gates cancel (`Cz` is an involution); two adjacent
///    `Cx(a,b)` gates (same control/target order) cancel likewise; two
///    adjacent `Rzz(a,b,t1)`/`Rzz(a,b,t2)` fuse into one
///    `Rzz(a,b,t1+t2)`. "Adjacent" here tolerates anything that itself
///    commutes with the two-qubit gate sitting between them (disjoint
///    qubits, or a floated `Rz` on a wire it's diagonal-compatible
///    with) -- **not** anything that had to be flushed there, since a
///    flushed gate is a real event that breaks the sandwich. This
///    matters concretely for `Cx`: `Cx(a,b).Rz(a,t).Cx(a,b)` really
///    does cancel around the control-side `Rz`, collapsing to just
///    `Rz(a,t)` -- but `Cx(a,b).Rz(b,t).Cx(a,b)` does **not** cancel;
///    it equals `Rzz(a,b,t)` (the same identity `IbmQSpec::push_two_qubit_zz`
///    builds in the other direction), so a genuinely-flushed target-side `Rz`
///    must invalidate the pending cancellation rather than be ignored.
///
/// Any rotation that nets out to an identity (mod `2*PI`) is dropped
/// entirely rather than emitted as a zero-angle gate.
pub fn optimize(bc: &mut BackendCircuit) {
    use std::collections::HashMap;

    let mut pending_rz: HashMap<usize, f64> = HashMap::new();
    // Index into `out` of the most recent `Rot` on a qubit, valid only
    // while nothing else has touched that qubit since.
    let mut last_rot: HashMap<usize, usize> = HashMap::new();
    // Index into `out` of the most recent two-qubit gate on an
    // unordered qubit pair, valid only while nothing that breaks the
    // sandwich has touched either wire since.
    let mut last_2q: HashMap<(usize, usize), usize> = HashMap::new();
    let mut out: Vec<Option<BackendGate>> = Vec::with_capacity(bc.gates.len());

    fn pair_key(a: usize, b: usize) -> (usize, usize) {
        if a < b { (a, b) } else { (b, a) }
    }

    // A gate that isn't itself part of a same-pair two-qubit
    // cancellation, but touches `q`, invalidates any two-qubit-gate
    // pair tracking that involves `q` -- something real just happened
    // on that wire.
    fn invalidate_pairs_touching(last_2q: &mut HashMap<(usize, usize), usize>, q: usize) {
        last_2q.retain(|&k, _| k.0 != q && k.1 != q);
    }

    // Invalidates every tracked pair touching `a` or `b` *except*
    // `keep` (the pair `(a,b)` itself, which the caller checks
    // separately for cancellation/fusion eligibility).
    fn invalidate_other_pairs(
        last_2q: &mut HashMap<(usize, usize), usize>,
        keep: (usize, usize),
        a: usize,
        b: usize,
    ) {
        last_2q.retain(|&k, _| k == keep || (k.0 != a && k.0 != b && k.1 != a && k.1 != b));
    }

    fn flush(
        q: usize,
        pending_rz: &mut HashMap<usize, f64>,
        last_rot: &mut HashMap<usize, usize>,
        last_2q: &mut HashMap<(usize, usize), usize>,
        out: &mut Vec<Option<BackendGate>>,
    ) {
        if let Some(theta) = pending_rz.remove(&q) {
            if !is_identity_angle(theta) {
                out.push(Some(BackendGate::Rz(q, wrap_angle(theta))));
                // A genuinely emitted Rz(q) is a real gate on wire q
                // now sitting in the output: any two-qubit pair
                // waiting on "nothing happened on q" (i.e. Cx with q
                // as target) is no longer eligible to cancel around it.
                invalidate_pairs_touching(last_2q, q);
            }
            last_rot.remove(&q);
        }
    }

    for g in bc.gates.drain(..) {
        match g {
            BackendGate::Rz(q, theta) => {
                *pending_rz.entry(q).or_insert(0.0) += theta;
                // An Rz between two Rot's on the same qubit breaks
                // their adjacency -- they're no longer eligible to
                // fuse with each other.
                last_rot.remove(&q);
            }
            BackendGate::Rot(q, theta) => {
                flush(q, &mut pending_rz, &mut last_rot, &mut last_2q, &mut out);
                if let Some(&idx) = last_rot.get(&q) {
                    if let Some(BackendGate::Rot(_, prev_theta)) = &mut out[idx] {
                        let merged = *prev_theta + theta;
                        if is_identity_angle(merged) {
                            // Leave a hole; the final sweep below strips
                            // it. (Removing mid-vector here would shift
                            // every other qubit's recorded indices and
                            // silently corrupt this pass.)
                            *prev_theta = 0.0;
                            last_rot.remove(&q);
                        } else {
                            *prev_theta = wrap_angle(merged);
                        }
                        continue;
                    }
                }
                out.push(Some(BackendGate::Rot(q, theta)));
                last_rot.insert(q, out.len() - 1);
                // A brand-new Rot is a real gate on q: it breaks any
                // same-pair two-qubit cancellation waiting on q too.
                invalidate_pairs_touching(&mut last_2q, q);
            }
            BackendGate::Cx(a, b) => {
                // Control wire (a) is diagonal-compatible: pending Rz
                // floats through untouched. Target wire (b) is not:
                // flush first (which, if it actually emits, also
                // invalidates any same-pair Cx-Cx cancellation below --
                // see the doc comment's Cx caveat).
                flush(b, &mut pending_rz, &mut last_rot, &mut last_2q, &mut out);
                last_rot.remove(&a);
                last_rot.remove(&b);

                let key = pair_key(a, b);
                invalidate_other_pairs(&mut last_2q, key, a, b);

                let mut cancelled = false;
                if let Some(&idx) = last_2q.get(&key) {
                    if let Some(BackendGate::Cx(pa, pb)) = out[idx] {
                        // Cx(a,b).Cx(a,b) == I -- but only the *same*
                        // control/target direction; Cx(a,b).Cx(b,a)
                        // is a different operator and must not cancel.
                        if pa == a && pb == b {
                            out[idx] = None;
                            last_2q.remove(&key);
                            cancelled = true;
                        }
                    }
                }
                if !cancelled {
                    out.push(Some(BackendGate::Cx(a, b)));
                    last_2q.insert(key, out.len() - 1);
                }
            }
            BackendGate::Cz(a, b) => {
                // Fully diagonal: pending Rz on either wire floats
                // through untouched, and (being an involution) two
                // adjacent Cz(a,b)'s cancel regardless of the stored
                // qubit order, since Cz(a,b) == Cz(b,a).
                last_rot.remove(&a);
                last_rot.remove(&b);

                let key = pair_key(a, b);
                invalidate_other_pairs(&mut last_2q, key, a, b);

                let mut cancelled = false;
                if let Some(&idx) = last_2q.get(&key) {
                    if matches!(out[idx], Some(BackendGate::Cz(..))) {
                        out[idx] = None;
                        last_2q.remove(&key);
                        cancelled = true;
                    }
                }
                if !cancelled {
                    out.push(Some(BackendGate::Cz(a, b)));
                    last_2q.insert(key, out.len() - 1);
                }
            }
            BackendGate::Rzz(a, b, theta) => {
                last_rot.remove(&a);
                last_rot.remove(&b);

                let key = pair_key(a, b);
                invalidate_other_pairs(&mut last_2q, key, a, b);

                let mut fused = false;
                if let Some(&idx) = last_2q.get(&key) {
                    if let Some(BackendGate::Rzz(_, _, prev_theta)) = &mut out[idx] {
                        // Rzz(a,b,t) is symmetric in a,b, so fusing
                        // regardless of stored qubit order is exact.
                        *prev_theta = wrap_angle(*prev_theta + theta);
                        fused = true;
                    }
                }
                if !fused {
                    out.push(Some(BackendGate::Rzz(a, b, theta)));
                    last_2q.insert(key, out.len() - 1);
                }
            }
            BackendGate::Measure(q, _) => {
                // A genuine event on wire q: flush any pending Rz
                // first (Measure isn't diagonal-compatible the way Cz
                // is -- it's not a unitary at all), and invalidate any
                // Rot/two-qubit-pair tracking waiting on q, since a
                // Measure breaks every one of those sandwiches.
                flush(q, &mut pending_rz, &mut last_rot, &mut last_2q, &mut out);
                last_rot.remove(&q);
                invalidate_pairs_touching(&mut last_2q, q);
                out.push(Some(g));
            }
        }
    }

    for q in pending_rz.keys().copied().collect::<Vec<_>>() {
        flush(q, &mut pending_rz, &mut last_rot, &mut last_2q, &mut out);
    }

    // Strip no-ops: explicit holes left by Cx/Cz cancellation, plus
    // any Rz/Rot/Rzz whose angle nets out to an identity mod 2*PI.
    bc.gates = out
        .into_iter()
        .flatten()
        .filter(|g| {
            !matches!(g,
                BackendGate::Rz(_, a) | BackendGate::Rot(_, a) | BackendGate::Rzz(_, _, a)
                if is_identity_angle(*a)
            )
        })
        .collect();
}

// `Backend::calibration` / `Backend::coupling_map` now live as inherent
// methods on `Backend` itself in `backend/spec.rs`, delegating to each
// backend's own `BackendSpec` implementation -- see that module's doc
// comment for why this moved out of a `match self { ... }` here.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emit;
    use crate::optimize::optimize as native_optimize;
    use rand::Rng;
    use sirraya_qutub::core::QuantumRegister;

    const TOL: f64 = 1e-9;

    fn randomized_register(num_qubits: usize) -> QuantumRegister {
        let mut reg = QuantumRegister::new(num_qubits).unwrap();
        let mut rng = rand::thread_rng();
        for q in 0..num_qubits {
            reg.apply_rz(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
            reg.apply_ry(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
            reg.apply_rz(q, rng.gen_range(0.0..std::f64::consts::TAU)).unwrap();
        }
        reg
    }

    /// Applies a `BackendCircuit` directly to a `QuantumRegister`,
    /// re-expressing each backend-native gate back in terms of qutub's
    /// own `apply_*` methods (`Cx` -> `apply_cnot`, `Cz` ->
    /// `apply_controlled_z`, `Rot` -> `apply_ry`/`apply_rx` depending on
    /// backend).
    fn apply_backend_circuit(bc: &BackendCircuit, reg: &mut QuantumRegister) {
        for g in &bc.gates {
            match *g {
                BackendGate::Rz(q, a) => reg.apply_rz(q, a).unwrap(),
                BackendGate::Rot(q, a) => match bc.backend.rot_axis() {
                    RotAxis::Ry => reg.apply_ry(q, a).unwrap(),
                    RotAxis::Rx => reg.apply_rx(q, a).unwrap(),
                },
                BackendGate::Cx(a, b) => reg.apply_cnot(a, b).unwrap(),
                BackendGate::Cz(a, b) => reg.apply_controlled_z(a, b).unwrap(),
                BackendGate::Rzz(a, b, t) => reg.apply_rzz(a, b, t).unwrap(),
                BackendGate::Measure(..) => panic!(
                    "apply_backend_circuit: Measure execution is blocked on confirming \
                     sirraya_qutub::core::QuantumRegister's measurement API (see P0.1's \
                     definition of done) -- no test in this file exercises Measure yet."
                ),
            }
        }
    }

    fn check_backend_matches(circuit: &Circuit, backend: Backend) {
        let mut direct = randomized_register(circuit.num_qubits);
        let mut lowered_reg = direct.clone();

        // Ground truth: same circuit, but through the already-tested
        // native {Rz,Ry,Rzz} + optimize path via emit::apply_to.
        let native = native_optimize(&crate::native::decompose(circuit));
        emit::apply_to(&native, &mut direct).unwrap();

        let bc = lower(circuit, backend);
        apply_backend_circuit(&bc, &mut lowered_reg);

        let fidelity = direct.fidelity(&lowered_reg).unwrap();
        assert!(
            (fidelity - 1.0).abs() < TOL,
            "backend {:?}: fidelity {} (gates: {:?})",
            backend,
            fidelity,
            bc.gates
        );
    }

    fn sample_circuit() -> Circuit {
        let mut c = Circuit::new(3);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 1))
            .push(Gate::Rz(2, 0.37))
            .push(Gate::Cx(1, 2))
            .push(Gate::T(0))
            .push(Gate::Ryy(0, 2, 0.91))
            .push(Gate::Swap(1, 2))
            .push(Gate::Cp(0, 1, 1.2));
        c
    }

    #[test]
    fn trapped_ion_matches_native_path() {
        check_backend_matches(&sample_circuit(), Backend::TrappedIon);
    }

    #[test]
    fn ibmq_matches_native_path() {
        check_backend_matches(&sample_circuit(), Backend::IbmQ);
    }

    #[test]
    fn rigetti_matches_native_path() {
        check_backend_matches(&sample_circuit(), Backend::Rigetti);
    }

    /// `sample_circuit`'s `Ryy(0, 2)` is already non-adjacent on a
    /// 3-qubit line, so `ibmq_matches_native_path`/`rigetti_matches_native_path`
    /// above already exercise routing implicitly. This test makes that
    /// explicit and checks the routing postcondition directly: on a
    /// wider, sparser circuit, every two-qubit `BackendGate` `lower`
    /// emits must land on physical qubits `Backend::coupling_map`
    /// actually allows to interact directly.
    #[test]
    fn lower_routes_distant_gates_onto_adjacent_qubits() {
        let mut c = Circuit::new(5);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 4))
            .push(Gate::Cp(1, 3, 0.8))
            .push(Gate::Ryy(2, 4, 0.5));

        for backend in [Backend::IbmQ, Backend::Rigetti] {
            let coupling = backend
                .coupling_map(c.num_qubits)
                .expect("IbmQ/Rigetti always have a coupling map");
            let bc = lower(&c, backend);
            for g in &bc.gates {
                let pair = match *g {
                    BackendGate::Cx(a, b) | BackendGate::Cz(a, b) => Some((a, b)),
                    BackendGate::Rzz(a, b, _) => Some((a, b)),
                    BackendGate::Rz(..) | BackendGate::Rot(..) => None,
                    BackendGate::Measure(..) => None,
                };
                if let Some((a, b)) = pair {
                    assert!(
                        coupling.is_adjacent(a, b),
                        "backend {:?}: two-qubit gate {:?} not on adjacent qubits",
                        backend,
                        g
                    );
                }
            }
            // Routing must not have changed the circuit's action.
            check_backend_matches(&c, backend);
        }
    }

    /// [`lower_with_coupling`] with an explicit real-style (irregular)
    /// map is what `submit_ibm.py --dump-coupling-map` +
    /// `CouplingMap::from_edges` is for: this pins down that routing
    /// actually happens against the *given* map (not the backend's own
    /// synthetic default), and that the routed circuit still lands
    /// only on that map's real edges.
    #[test]
    fn lower_with_coupling_routes_against_the_given_map_not_the_synthetic_default() {
        // A deliberately irregular 5-qubit line with one qubit (3)
        // dropped from the middle -- nothing like heavy_hex_for(5) or
        // square_grid_for(5) -- exactly the kind of real-device-style
        // topology `from_edges` exists for.
        let coupling = crate::coupling::CouplingMap::from_edges(
            5,
            [(0, 1), (1, 2), (2, 4)],
        )
        .unwrap();

        let mut c = Circuit::new(5);
        c.push(Gate::H(0)).push(Gate::Cx(0, 4));

        let bc = lower_with_coupling(&c, Backend::IbmQ, Some(&coupling));
        for g in &bc.gates {
            let pair = match *g {
                BackendGate::Cx(a, b) | BackendGate::Cz(a, b) => Some((a, b)),
                BackendGate::Rzz(a, b, _) => Some((a, b)),
                _ => None,
            };
            if let Some((a, b)) = pair {
                assert!(
                    coupling.is_adjacent(a, b),
                    "two-qubit gate {:?} not on an edge of the explicitly-given map",
                    g
                );
            }
        }

        // Same action as an unrouted/no-topology lowering, up to the
        // routing-inserted Swaps -- checked the same way
        // lower_routes_distant_gates_onto_adjacent_qubits does for the
        // synthetic-default path.
        check_backend_matches(&c, Backend::IbmQ);
    }

    /// `lower_with_coupling(..., None)` must skip routing entirely,
    /// same as `lower` already does for `TrappedIon` (whose
    /// `coupling_map` is always `None`) -- i.e. it's a real, distinct
    /// choice from "use the synthetic default", not an error case.
    #[test]
    fn lower_with_coupling_none_skips_routing() {
        let mut c = Circuit::new(3);
        c.push(Gate::H(0)).push(Gate::Cx(0, 2));

        let bc = lower_with_coupling(&c, Backend::IbmQ, None);
        // Cx(0, 2) is far apart on IbmQ's own default heavy-hex map,
        // so if this had routed, we'd very likely see a Swap-derived
        // extra Cx pair. With no coupling map, it should lower with
        // exactly the same native two-qubit gate count as an
        // unrouted, source-level Cx costs on IbmQ (1, per
        // push_native_cx's fast path).
        let (_, two_count) = bc.gate_counts();
        assert_eq!(
            two_count, 1,
            "no coupling map given -- Cx(0,2) should lower directly with no routing SWAPs: {:?}",
            bc.gates
        );
    }

    #[test]
    fn lower_and_lower_with_coupling_agree_when_given_the_backends_own_default_map() {
        let c = sample_circuit();
        for backend in [Backend::IbmQ, Backend::Rigetti] {
            let default_map = backend.coupling_map(c.num_qubits);
            let via_lower = lower(&c, backend);
            let via_explicit = lower_with_coupling(&c, backend, default_map.as_ref());
            assert_eq!(
                via_lower.gates, via_explicit.gates,
                "backend {:?}: lower() should just be lower_with_coupling() called with \
                 the backend's own default map",
                backend
            );
        }
    }

    #[test]
    fn measure_survives_lowering_on_every_backend() {
        for backend in [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti] {
            let mut c = Circuit::new(2);
            c.num_clbits = 1;
            c.push(Gate::H(0)).push(Gate::Measure(0, 0));
            let bc = lower(&c, backend);
            assert_eq!(bc.num_clbits, 1, "backend {:?}: num_clbits must survive lowering", backend);
            assert!(
                matches!(bc.gates.last(), Some(BackendGate::Measure(0, 0))),
                "backend {:?}: Measure must survive lowering as the last gate, got {:?}",
                backend,
                bc.gates
            );
        }
    }

    #[test]
    fn measure_is_not_counted_in_backend_gate_counts() {
        let mut c = Circuit::new(1);
        c.num_clbits = 1;
        c.push(Gate::Measure(0, 0));
        let bc = lower(&c, Backend::IbmQ);
        assert_eq!(bc.gate_counts(), (0, 0), "Measure must not be priced as a unitary gate");
    }

    #[test]
    fn resynthesize_flushes_pending_rotation_before_measure() {
        // Rz then Rot pending on qubit 0, then a Measure: resynthesize
        // must flush the accumulated single-qubit unitary onto qubit 0
        // before the Measure, the same way it already does before a
        // two-qubit gate touching that wire.
        let mut bc = BackendCircuit::new(Backend::IbmQ, 1);
        bc.push(BackendGate::Rz(0, 0.3));
        bc.push(BackendGate::Rot(0, 0.4));
        bc.push(BackendGate::Measure(0, 0));
        resynthesize(&mut bc);
        assert!(
            matches!(bc.gates.last(), Some(BackendGate::Measure(0, 0))),
            "Measure must remain the last gate after resynthesize: {:?}",
            bc.gates
        );
        assert!(
            bc.gates.len() > 1,
            "the pending Rz/Rot must have been flushed onto qubit 0 before the Measure: {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_never_drops_or_reorders_measure() {
        let mut bc = BackendCircuit::new(Backend::IbmQ, 1);
        bc.push(BackendGate::Rz(0, 0.0)); // would be dropped on its own
        bc.push(BackendGate::Measure(0, 0));
        optimize(&mut bc);
        assert_eq!(bc.gates, vec![BackendGate::Measure(0, 0)]);
    }

    #[test]
    fn trapped_ion_lowering_is_unaffected_by_routing() {
        // TrappedIon has no coupling map, so a distant gate should
        // lower exactly as it did before routing existed.
        let mut c = Circuit::new(5);
        c.push(Gate::Cx(0, 4));
        assert!(Backend::TrappedIon.coupling_map(c.num_qubits).is_none());
        check_backend_matches(&c, Backend::TrappedIon);
    }

    #[test]
    fn ibmq_rzz_costs_one_cx_pair() {
        let mut c = Circuit::new(2);
        c.push(Gate::Rzz(0, 1, 0.5));
        let bc = lower(&c, Backend::IbmQ);
        let cx_count = bc.gates.iter().filter(|g| matches!(g, BackendGate::Cx(..))).count();
        assert_eq!(cx_count, 2, "Rzz should lower to exactly 2 Cx on IbmQ");
    }

    #[test]
    fn rigetti_and_ibmq_use_the_same_two_qubit_gate_count_for_rzz() {
        // Both go through the same Cx(a,b).Rz(b,theta).Cx(a,b)
        // intermediate (2 two-qubit gates); Rigetti just re-expresses
        // each of those 2 Cx's as 1 Cz (via Cx == H.Cz.H), so the
        // two-qubit *count* comes out equal -- 2 Cx vs 2 Cz, not more.
        let mut c = Circuit::new(2);
        c.push(Gate::Rzz(0, 1, 0.5));
        let (_, ibmq_two) = lower(&c, Backend::IbmQ).gate_counts();
        let (_, rigetti_two) = lower(&c, Backend::Rigetti).gate_counts();
        assert_eq!(
            ibmq_two, rigetti_two,
            "expected equal 2Q gate counts (2 Cx vs 2 Cz) for the same Rzz: {} vs {}",
            ibmq_two, rigetti_two
        );
    }

    #[test]
    fn rigetti_rzz_costs_more_single_qubit_gates_than_ibmq() {
        // Rigetti still pays more than IbmQ's native Cx (it needs H's
        // at all), just roughly half of what the naive 4-H expansion
        // would cost -- see rigetti_rzz_uses_only_two_h_conjugations.
        let mut c = Circuit::new(2);
        c.push(Gate::Rzz(0, 1, 0.5));
        let (ibmq_single, _) = lower(&c, Backend::IbmQ).gate_counts();
        let (rigetti_single, _) = lower(&c, Backend::Rigetti).gate_counts();
        assert!(
            rigetti_single > ibmq_single,
            "Rigetti's Cx-via-Cz should need more single-qubit gates than IbmQ's native Cx: {} vs {}",
            rigetti_single,
            ibmq_single
        );
    }

    #[test]
    fn h_rz_h_equals_rx_identity_holds_exactly() {
        // Direct check of identity 3 in this module's doc comment:
        // H(0).Rz(0,theta).H(0) should match Rx(0,theta) at fidelity 1,
        // independent of any circuit that uses it.
        let mut rng = rand::thread_rng();
        for _ in 0..8 {
            let theta: f64 = rng.gen_range(-std::f64::consts::TAU..std::f64::consts::TAU);

            let mut lhs = randomized_register(1);
            let mut rhs = lhs.clone();

            lhs.apply_hadamard(0).unwrap();
            lhs.apply_rz(0, theta).unwrap();
            lhs.apply_hadamard(0).unwrap();

            rhs.apply_rx(0, theta).unwrap();

            let fidelity = lhs.fidelity(&rhs).unwrap();
            assert!(
                (fidelity - 1.0).abs() < TOL,
                "H.Rz(theta).H should equal Rx(theta): fidelity {} at theta {}",
                fidelity,
                theta
            );
        }
    }

    #[test]
    fn rigetti_rzz_uses_only_two_h_conjugations() {
        // Regression check for the fix: Rigetti's Rzz lowering should
        // use the shortened H.Cz.Rx.Cz.H form (2 H "sides"), not the
        // naive 4-H expansion. A single Rzz's push_h call emits at
        // least one BackendGate::Rz for the H's zyz `beta`/`delta`
        // component (m_h() always has a nonzero beta), so counting Rz
        // gates whose surrounding structure came from push_h is fiddly
        // -- instead this just pins the total single-qubit count to be
        // meaningfully smaller than double what a naive-4H version
        // would need, using IbmQ's 1-single-gate Rzz as the cheap
        // reference point and asserting Rigetti's overhead is bounded
        // rather than unbounded as circuits with many Rzz gates grow.
        let mut c = Circuit::new(2);
        c.push(Gate::Rzz(0, 1, 0.5));
        let rigetti_single = lower(&c, Backend::Rigetti).gate_counts().0;
        let one_h_cost = lower(&{
            let mut hc = Circuit::new(1);
            hc.push(Gate::H(0));
            hc
        }, Backend::Rigetti).gate_counts().0;

        // Exactly 2 H's-worth of single-qubit overhead, plus the 1
        // Rot(theta) in the middle -- not 4 H's-worth.
        assert_eq!(
            rigetti_single,
            2 * one_h_cost + 1,
            "expected exactly 2 H-conjugations + 1 Rot(theta) for Rigetti's Rzz, got {} single-qubit gates (1 H costs {})",
            rigetti_single,
            one_h_cost
        );
    }

    #[test]
    fn resynthesize_collapses_a_long_single_qubit_run_to_at_most_three_gates() {
        // H.T.H on one qubit, no two-qubit gate in between: exactly the
        // case optimize()'s adjacent-pair-only Rot fusion can't
        // collapse (each H/T contributes its own Rz.Rot.Rz block with
        // real Rz's between blocks), but the whole run is still one
        // single-qubit unitary and must resynthesize to <= 3 gates.
        let mut c = Circuit::new(1);
        c.push(Gate::H(0)).push(Gate::T(0)).push(Gate::H(0));
        let bc = lower(&c, Backend::IbmQ);
        assert!(
            bc.gates.len() <= 3,
            "a single-qubit run should resynthesize to at most 3 gates, got {}: {:?}",
            bc.gates.len(),
            bc.gates
        );
    }

    #[test]
    fn resynthesize_matches_original_action_on_a_synthetic_multi_block_run() {
        // Directly build a BackendCircuit out of several independent
        // Rz/Rot blocks stacked on one qubit -- the shape
        // push_ry_via_rx/push_h leave behind for several source gates
        // in a row -- and confirm resynthesize both shrinks it to <= 3
        // gates and leaves the actual unitary unchanged, checked
        // against the real simulator the same way every other
        // correctness check in this crate is.
        let mut bc = BackendCircuit::new(Backend::IbmQ, 1);
        bc.push(BackendGate::Rot(0, FRAC_PI_2));
        bc.push(BackendGate::Rz(0, 0.6));
        bc.push(BackendGate::Rot(0, -FRAC_PI_2));
        bc.push(BackendGate::Rz(0, 0.2));
        bc.push(BackendGate::Rot(0, 1.1));
        bc.push(BackendGate::Rz(0, -0.4));

        let mut direct = randomized_register(1);
        let mut resynth_reg = direct.clone();
        apply_backend_circuit(&bc, &mut direct);

        let mut resynthesized = bc.clone();
        resynthesize(&mut resynthesized);
        assert!(
            resynthesized.gates.len() <= 3,
            "expected the whole run to collapse to <= 3 gates, got {}: {:?}",
            resynthesized.gates.len(),
            resynthesized.gates
        );
        apply_backend_circuit(&resynthesized, &mut resynth_reg);

        let fidelity = direct.fidelity(&resynth_reg).unwrap();
        assert!(
            (fidelity - 1.0).abs() < TOL,
            "resynthesize changed the circuit's action: fidelity {} (result: {:?})",
            fidelity,
            resynthesized.gates
        );
    }

    #[test]
    fn resynthesize_shrinks_the_sample_circuit_on_rigetti() {
        // The concrete win this pass is for: Rigetti's single-qubit
        // count on a denser multi-gate circuit should end up bounded
        // by roughly 3 gates per two-qubit-gate boundary, not grow
        // with how many source gates happened to land in between.
        let c = sample_circuit();
        let bc = lower(&c, Backend::Rigetti);
        let (single, two) = bc.gate_counts();
        assert!(
            single <= 3 * (two + c.num_qubits),
            "expected single-qubit count ({}) bounded by ~3 per 2Q-gate boundary \
             ({} two-qubit gates, {} qubits), got a much larger count -- \
             resynthesize may not be collapsing runs as expected: {:?}",
            single,
            two,
            c.num_qubits,
            bc.gates
        );
    }

    #[test]
    fn optimize_merges_adjacent_same_axis_rotations() {
        let mut bc = BackendCircuit::new(Backend::IbmQ, 1);
        bc.push(BackendGate::Rz(0, 0.3));
        bc.push(BackendGate::Rz(0, 0.4));
        optimize(&mut bc);
        assert_eq!(bc.gates, vec![BackendGate::Rz(0, wrap_angle(0.7))]);
    }

    #[test]
    fn optimize_drops_rotations_that_cancel_to_identity() {
        let mut bc = BackendCircuit::new(Backend::IbmQ, 1);
        bc.push(BackendGate::Rz(0, 1.2));
        bc.push(BackendGate::Rz(0, -1.2));
        optimize(&mut bc);
        assert!(
            bc.gates.is_empty(),
            "opposite Rz's should cancel entirely, got {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_floats_rz_through_diagonal_two_qubit_gates() {
        // Rz(0) . Cz(0,1) . Rz(0) should collapse the two Rz's into
        // one, since Rz is diagonal and commutes with Cz on either wire.
        let mut bc = BackendCircuit::new(Backend::Rigetti, 2);
        bc.push(BackendGate::Rz(0, 0.3));
        bc.push(BackendGate::Cz(0, 1));
        bc.push(BackendGate::Rz(0, 0.4));
        optimize(&mut bc);
        assert_eq!(
            bc.gates,
            vec![BackendGate::Cz(0, 1), BackendGate::Rz(0, wrap_angle(0.7))],
            "the two Rz(0)'s should fuse across the commuting Cz: {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_does_not_float_rz_through_cx_target_wire() {
        // Rz(1) . Cx(0,1) . Rz(1) must NOT merge: qubit 1 is Cx's
        // target, and Rz doesn't commute with the X-conjugation Cx
        // applies there.
        let mut bc = BackendCircuit::new(Backend::IbmQ, 2);
        bc.push(BackendGate::Rz(1, 0.3));
        bc.push(BackendGate::Cx(0, 1));
        bc.push(BackendGate::Rz(1, 0.4));
        optimize(&mut bc);
        assert_eq!(
            bc.gates,
            vec![
                BackendGate::Rz(1, 0.3),
                BackendGate::Cx(0, 1),
                BackendGate::Rz(1, 0.4),
            ],
            "Rz on Cx's target wire must not merge across the Cx: {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_floats_rz_through_cx_control_wire() {
        // Rz(0) . Cx(0,1) . Rz(0) SHOULD merge: qubit 0 is Cx's
        // control, which is diagonal-compatible with Rz.
        let mut bc = BackendCircuit::new(Backend::IbmQ, 2);
        bc.push(BackendGate::Rz(0, 0.3));
        bc.push(BackendGate::Cx(0, 1));
        bc.push(BackendGate::Rz(0, 0.4));
        optimize(&mut bc);
        assert_eq!(
            bc.gates,
            vec![BackendGate::Cx(0, 1), BackendGate::Rz(0, wrap_angle(0.7))],
            "Rz on Cx's control wire should fuse across the Cx: {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_does_not_merge_rot_across_intervening_rz() {
        // Rot(pi/2) . Rz(theta) . Rot(-pi/2) is a real ZYZ-style
        // sandwich (e.g. from push_ry_via_rx) -- the two Rot's must
        // NOT cancel just because their angles are opposite, since
        // Rz(theta) sits meaningfully between them.
        let mut bc = BackendCircuit::new(Backend::IbmQ, 1);
        bc.push(BackendGate::Rot(0, FRAC_PI_2));
        bc.push(BackendGate::Rz(0, 0.6));
        bc.push(BackendGate::Rot(0, -FRAC_PI_2));
        optimize(&mut bc);
        assert_eq!(
            bc.gates,
            vec![
                BackendGate::Rot(0, FRAC_PI_2),
                BackendGate::Rz(0, 0.6),
                BackendGate::Rot(0, -FRAC_PI_2),
            ],
            "Rot's either side of a real Rz must survive untouched: {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_cancels_adjacent_same_direction_cx() {
        let mut bc = BackendCircuit::new(Backend::IbmQ, 2);
        bc.push(BackendGate::Cx(0, 1));
        bc.push(BackendGate::Cx(0, 1));
        optimize(&mut bc);
        assert!(
            bc.gates.is_empty(),
            "Cx(0,1).Cx(0,1) == I, got {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_does_not_cancel_opposite_direction_cx() {
        // Cx(0,1) and Cx(1,0) are different operators; they must not
        // be treated as cancelling.
        let mut bc = BackendCircuit::new(Backend::IbmQ, 2);
        bc.push(BackendGate::Cx(0, 1));
        bc.push(BackendGate::Cx(1, 0));
        optimize(&mut bc);
        assert_eq!(bc.gates, vec![BackendGate::Cx(0, 1), BackendGate::Cx(1, 0)]);
    }

    #[test]
    fn optimize_cancels_cx_pair_around_control_side_rz() {
        // Cx(0,1).Rz(0,t).Cx(0,1) == Rz(0,t): the control wire (0) is
        // untouched by the conjugation, so the Cx's cancel around it.
        let mut bc = BackendCircuit::new(Backend::IbmQ, 2);
        bc.push(BackendGate::Cx(0, 1));
        bc.push(BackendGate::Rz(0, 0.4));
        bc.push(BackendGate::Cx(0, 1));
        optimize(&mut bc);
        assert_eq!(
            bc.gates,
            vec![BackendGate::Rz(0, 0.4)],
            "Cx's should cancel around a control-side Rz, leaving just the Rz: {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_does_not_cancel_cx_pair_around_target_side_rz() {
        // Cx(0,1).Rz(1,t).Cx(0,1) == Rzz(0,1,t), NOT identity around
        // Rz(1,t) -- the target wire's Rz does not commute with Cx.
        // This is the exact trap: naively cancelling here would drop
        // a real ZZ coupling from the circuit.
        let mut bc = BackendCircuit::new(Backend::IbmQ, 2);
        bc.push(BackendGate::Cx(0, 1));
        bc.push(BackendGate::Rz(1, 0.4));
        bc.push(BackendGate::Cx(0, 1));
        optimize(&mut bc);
        assert_eq!(
            bc.gates,
            vec![
                BackendGate::Cx(0, 1),
                BackendGate::Rz(1, 0.4),
                BackendGate::Cx(0, 1),
            ],
            "Cx's must NOT cancel around a target-side Rz -- this sandwich is Rzz, not identity: {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_cancels_adjacent_cz() {
        let mut bc = BackendCircuit::new(Backend::Rigetti, 2);
        bc.push(BackendGate::Cz(0, 1));
        bc.push(BackendGate::Cz(0, 1));
        optimize(&mut bc);
        assert!(bc.gates.is_empty(), "Cz(0,1).Cz(0,1) == I, got {:?}", bc.gates);
    }

    #[test]
    fn optimize_cancels_cz_pair_around_rz_on_either_wire() {
        // Cz is fully diagonal, so unlike Cx, a genuine Rz on *either*
        // wire between two Cz(a,b)'s still allows cancellation.
        let mut bc = BackendCircuit::new(Backend::Rigetti, 2);
        bc.push(BackendGate::Cz(0, 1));
        bc.push(BackendGate::Rz(1, 0.4));
        bc.push(BackendGate::Cz(0, 1));
        optimize(&mut bc);
        assert_eq!(
            bc.gates,
            vec![BackendGate::Rz(1, 0.4)],
            "Cz's should cancel around an Rz on either wire: {:?}",
            bc.gates
        );
    }

    #[test]
    fn optimize_fuses_adjacent_rzz_on_same_pair() {
        let mut bc = BackendCircuit::new(Backend::TrappedIon, 2);
        bc.push(BackendGate::Rzz(0, 1, 0.3));
        bc.push(BackendGate::Rzz(0, 1, 0.5));
        optimize(&mut bc);
        assert_eq!(bc.gates, vec![BackendGate::Rzz(0, 1, wrap_angle(0.8))]);
    }

    #[test]
    fn optimize_does_not_fuse_rzz_across_an_intervening_rot() {
        let mut bc = BackendCircuit::new(Backend::TrappedIon, 2);
        bc.push(BackendGate::Rzz(0, 1, 0.3));
        bc.push(BackendGate::Rot(0, 0.1));
        bc.push(BackendGate::Rzz(0, 1, 0.5));
        optimize(&mut bc);
        assert_eq!(
            bc.gates,
            vec![
                BackendGate::Rzz(0, 1, 0.3),
                BackendGate::Rot(0, 0.1),
                BackendGate::Rzz(0, 1, 0.5),
            ],
            "a real Rot between them must block fusion: {:?}",
            bc.gates
        );
    }

    #[test]
    fn each_backend_calibration_gives_a_fidelity_estimate() {
        use crate::fidelity::estimate_backend_circuit_fidelity;
        for backend in [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti] {
            let bc = lower(&sample_circuit(), backend);
            let cal = backend.calibration();
            let fidelity = estimate_backend_circuit_fidelity(&bc, &cal);
            assert!(
                fidelity > 0.0 && fidelity <= 1.0,
                "backend {:?}: fidelity estimate {} out of range",
                backend,
                fidelity
            );
        }
    }
}