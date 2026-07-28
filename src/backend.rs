//! Lowers a [`Circuit`] to a specific backend's native gate set, instead
//! of only the trapped-ion-style `{Rz, Ry, Rzz}` target in [`crate::native`].
//!
//! # Backends implemented
//! - [`Backend::TrappedIon`] -- `{Rz, Ry, Rzz}`. Delegates straight to
//!   [`crate::native::decompose`] (unchanged, already tested).
//! - [`Backend::IbmQ`] -- `{Rz, Rx, Cx}`, modeling IBM's superconducting
//!   basis (virtual-Z framing + a native two-qubit `CNOT`).
//! - [`Backend::Rigetti`] -- `{Rz, Rx, Cz}`, modeling Rigetti's
//!   superconducting basis (`CZ`-native rather than `CNOT`-native).
//!
//! Two new circuit identities do the actual work here, on top of the
//! ones already in `native.rs`:
//! 1. `Ry(theta) == Rx(-pi/2) . Rz(theta) . Rx(pi/2)` -- reused
//!    directly from the `RYY` decomposition in `native.rs` (same
//!    Y = Rx(-pi/2).Z.Rx(pi/2) fact, exponentiated), so IBMQ/Rigetti's
//!    single-qubit gates reuse the *same* ZYZ synthesis as the
//!    trapped-ion target and just re-express the resulting `Ry` calls.
//! 2. `Rzz(a, b, theta) == Cx(a, b) . Rz(b, theta) . Cx(a, b)` -- new,
//!    and the reason `Cx` is exactly as cheap on `IbmQ` as `Rzz` is on
//!    `TrappedIon` (one native two-qubit gate), while every *other*
//!    two-qubit gate that isn't already `Cx` costs more.
//!
//! `Rigetti` (`Cz`-native, no native `Cx`) lowers the same
//! `Cx(a,b).Rz(b,theta).Cx(a,b)` intermediate, but *not* by naively
//! substituting `Cx(a,b) == H(b).Cz(a,b).H(b)` twice (which would cost
//! 4 `H`'s). Instead it uses a third identity:
//! 3. `H(b) . Rz(b, theta) . H(b) == Rx(b, theta)` -- exact, because
//!    conjugating the Pauli `Z` generator by `H` gives `X`
//!    (`H.Z.H == X`), so conjugating `Rz(theta) = exp(-i*theta*Z/2)` by
//!    `H` gives `Rx(theta) = exp(-i*theta*X/2)` at the operator-
//!    exponential level. Substituting this into the naive
//!    `H.Cz.H . Rz(theta) . H.Cz.H` expansion collapses the *middle*
//!    `H . Rz(theta) . H` into a single native `Rx` (Rigetti's `Rot`),
//!    leaving `H(b) . Cz(a,b) . Rx(b,theta) . Cz(a,b) . H(b)` -- 2 `H`'s
//!    instead of 4, same 2 `Cz`'s as before. `push_rzz`'s `Rigetti` arm
//!    below builds exactly this shorter form directly, rather than
//!    calling a generic `Cx`-via-`Cz` helper twice and relying on a
//!    peephole pass to notice the cancellation after the fact.
//!
//! # What's not here: Pasqal (neutral atoms)
//! Neutral-atom platforms (Pasqal, and analog/digital Rydberg-blockade
//! devices generally) aren't a fourth entry in this same enum on
//! purpose. Their native "two-qubit gate" is a blockade interaction
//! between whichever atoms are currently within blockade radius of each
//! other in a *movable, laser-tweezer-defined* 2D/3D layout -- so
//! "compiling to Pasqal's native gates" is inseparable from *placing*
//! the atoms and routing which pairs are ever simultaneously in
//! blockade range, which is a materially different problem from
//! "express this unitary in terms of a fixed two-qubit gate" (the
//! problem this module and `native.rs` solve). Pasqal does also expose
//! a "digital" mode with a fixed local `CZ`-like gate (making it
//! superficially similar to `Rigetti` here), but modeling it correctly
//! still needs blockade-radius/layout constraints this crate doesn't
//! have -- shipping a `Backend::Pasqal` that reused the `Rigetti` path
//! under a different name would be presenting an untested, physically
//! incomplete backend as equivalent to the two above, which were tested
//! the same way `native.rs` was. Left as a follow-on.

use crate::ir::{Circuit, Gate};
use std::f64::consts::FRAC_PI_2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    TrappedIon,
    IbmQ,
    Rigetti,
}

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
    fn push(&mut self, g: BackendGate) {
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

const EPS: f64 = 1e-9;

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
pub fn lower(circuit: &Circuit, backend: Backend) -> BackendCircuit {
    let routed_storage;
    let circuit: &Circuit = match backend.coupling_map(circuit.num_qubits) {
        Some(coupling) => {
            routed_storage = crate::route::route(circuit, &coupling);
            &routed_storage
        }
        None => circuit,
    };

    match backend {
        Backend::TrappedIon => {
            // Already-tested path: reuse native.rs verbatim.
            let native = crate::native::decompose(circuit);
            let mut bc = BackendCircuit::new(backend, circuit.num_qubits);
            bc.num_clbits = native.num_clbits;
            for g in &native.gates {
                bc.push(match *g {
                    crate::native::NativeGate::Rz(q, a) => BackendGate::Rz(q, a),
                    crate::native::NativeGate::Ry(q, a) => BackendGate::Rot(q, a),
                    crate::native::NativeGate::Rzz(a, b, t) => BackendGate::Rzz(a, b, t),
                    crate::native::NativeGate::Measure(q, c) => BackendGate::Measure(q, c),
                });
            }
            optimize(&mut bc);
            bc
        }
        Backend::IbmQ | Backend::Rigetti => {
            // Reuse the same Rz/Ry/Rzz canonical form (native.rs), then
            // re-express each gate in terms of this backend's native
            // Rx/Cx (IbmQ) or Rx/Cz (Rigetti).
            let native = crate::native::decompose(circuit);
            let mut bc = BackendCircuit::new(backend, circuit.num_qubits);
            bc.num_clbits = native.num_clbits;
            for g in &native.gates {
                match *g {
                    crate::native::NativeGate::Rz(q, a) => bc.push(BackendGate::Rz(q, a)),
                    crate::native::NativeGate::Ry(q, a) => push_ry_via_rx(&mut bc, q, a),
                    crate::native::NativeGate::Rzz(a, b, t) => push_rzz(&mut bc, backend, a, b, t),
                    crate::native::NativeGate::Measure(q, c) => bc.push(BackendGate::Measure(q, c)),
                }
            }
            // The per-gate ZYZ re-expansion above (especially `H` inside
            // `push_rzz`'s Rigetti arm, and every `Ry` via
            // `push_ry_via_rx`) is emitted independently of its
            // neighbors, so plenty of adjacent/commuting single-qubit
            // rotations are left unmerged -- and worse, `optimize`'s
            // `Rot` fusion only ever merges two *literally adjacent*
            // `Rot`s; a run like `Rot(pi/2).Rz(theta).Rot(-pi/2)` (one
            // `push_ry_via_rx` block) sitting next to another such
            // block from a neighboring source gate is really one
            // single-qubit unitary wearing up to 6+ gates, and no
            // adjacent-pair rule ever collapses that. `resynthesize`
            // (see its doc comment) closes that gap by collapsing the
            // whole run algebraically instead of pattern-matching
            // adjacent pairs; running it back-to-back with `optimize`
            // to a fixed point lets a two-qubit cancellation freed up
            // by one pass expose a longer single-qubit run for the
            // other, and vice versa.
            loop {
                let before = bc.gates.len();
                resynthesize(&mut bc);
                optimize(&mut bc);
                if bc.gates.len() == before {
                    break;
                }
            }
            bc
        }
    }
}

/// `Ry(theta) == Rx(-pi/2) . Rz(theta) . Rx(pi/2)` (apply `Rx(pi/2)`
/// first, then `Rz(theta)`, then `Rx(-pi/2)` last). See this module's
/// doc comment, identity 1.
fn push_ry_via_rx(bc: &mut BackendCircuit, q: usize, theta: f64) {
    bc.push(BackendGate::Rot(q, FRAC_PI_2));
    bc.push(BackendGate::Rz(q, theta));
    bc.push(BackendGate::Rot(q, -FRAC_PI_2));
}

/// `Rzz(a, b, theta) == Cx(a, b) . Rz(b, theta) . Cx(a, b)`. On `IbmQ`
/// this is used directly (identity 2). On `Rigetti` the *shortened*
/// form is built directly -- `H(b) . Cz(a,b) . Rx(b,theta) . Cz(a,b) .
/// H(b)` -- via identity 3 in this module's doc comment, rather than
/// substituting `Cx(a,b) == H(b).Cz(a,b).H(b)` twice and paying for 4
/// `H`'s when 2 suffice.
fn push_rzz(bc: &mut BackendCircuit, backend: Backend, a: usize, b: usize, theta: f64) {
    if theta.abs() < EPS {
        return;
    }
    match backend {
        Backend::IbmQ => {
            bc.push(BackendGate::Cx(a, b));
            bc.push(BackendGate::Rz(b, theta));
            bc.push(BackendGate::Cx(a, b));
        }
        Backend::Rigetti => {
            // H(b).Cz(a,b).H(b) . Rz(b,theta) . H(b).Cz(a,b).H(b)
            //   == H(b).Cz(a,b) . [H(b).Rz(b,theta).H(b)] . Cz(a,b).H(b)
            //   == H(b).Cz(a,b) . Rx(b,theta) . Cz(a,b).H(b)     (identity 3)
            push_h(bc, b);
            bc.push(BackendGate::Cz(a, b));
            bc.push(BackendGate::Rot(b, theta)); // Rot == Rx on Rigetti
            bc.push(BackendGate::Cz(a, b));
            push_h(bc, b);
        }
        Backend::TrappedIon => unreachable!("push_rzz only called for IbmQ/Rigetti"),
    }
}

/// Lowers a single `H(q)` by re-running it through the *same*
/// `native::decompose` + `push_ry_via_rx` path every other single-qubit
/// gate takes here (rather than hand-deriving `H`'s specific `Rz`/`Ry`
/// angles a second time and risking a fresh sign error the way the
/// first version of `native.rs`'s ZYZ synthesis did).
fn push_h(bc: &mut BackendCircuit, q: usize) {
    let mut h_circuit = Circuit::new(q + 1);
    h_circuit.push(Gate::H(q));
    let canonical = crate::native::decompose(&h_circuit);
    for g in &canonical.gates {
        match *g {
            crate::native::NativeGate::Rz(qq, a) => bc.push(BackendGate::Rz(qq, a)),
            crate::native::NativeGate::Ry(qq, a) => push_ry_via_rx(bc, qq, a),
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

    let backend = bc.backend;

    fn single_qubit_matrix(backend: Backend, g: BackendGate) -> Option<(usize, Mat2)> {
        match g {
            BackendGate::Rz(q, a) => Some((q, m_rz(a))),
            BackendGate::Rot(q, a) => {
                let m = match backend {
                    Backend::TrappedIon => m_ry(a),
                    Backend::IbmQ | Backend::Rigetti => m_rx(a),
                };
                Some((q, m))
            }
            _ => None,
        }
    }

    fn emit_synth(out: &mut Vec<BackendGate>, q: usize, m: Mat2, backend: Backend) {
        let (delta, gamma, beta) = zyz_decompose(m);
        let (first_z, mid, last_z) = match backend {
            Backend::TrappedIon => (delta, gamma, beta),
            Backend::IbmQ | Backend::Rigetti => (delta - FRAC_PI_2, gamma, beta + FRAC_PI_2),
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

    fn flush(q: usize, acc: &mut HashMap<usize, Mat2>, out: &mut Vec<BackendGate>, backend: Backend) {
        if let Some(m) = acc.remove(&q) {
            emit_synth(out, q, m, backend);
        }
    }

    let mut acc: HashMap<usize, Mat2> = HashMap::new();
    let mut out: Vec<BackendGate> = Vec::with_capacity(bc.gates.len());

    for g in bc.gates.drain(..) {
        if let Some((q, m)) = single_qubit_matrix(backend, g) {
            let entry = acc.entry(q).or_insert_with(m_identity);
            *entry = matmul(m, *entry);
            continue;
        }
        match g {
            BackendGate::Cx(a, b) | BackendGate::Cz(a, b) => {
                flush(a, &mut acc, &mut out, backend);
                flush(b, &mut acc, &mut out, backend);
                out.push(g);
            }
            BackendGate::Rzz(a, b, _) => {
                flush(a, &mut acc, &mut out, backend);
                flush(b, &mut acc, &mut out, backend);
                out.push(g);
            }
            BackendGate::Measure(q, _) => {
                // A real event on wire q: any pending single-qubit
                // rotation accumulated for it must be emitted first,
                // the same as for a two-qubit gate touching q.
                flush(q, &mut acc, &mut out, backend);
                out.push(g);
            }
            BackendGate::Rz(..) | BackendGate::Rot(..) => unreachable!("handled above"),
        }
    }
    for q in acc.keys().copied().collect::<Vec<_>>() {
        flush(q, &mut acc, &mut out, backend);
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
/// [`push_ry_via_rx`] / [`push_h`]), so adjacent single-qubit rotations
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
///    it equals `Rzz(a,b,t)` (the same identity `push_rzz` builds in
///    the other direction), so a genuinely-flushed target-side `Rz`
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

impl Backend {
    /// The [`crate::fidelity::PublishedCalibration`] matching this
    /// backend's modeled hardware, so a `BackendCircuit`'s fidelity can
    /// be estimated with the right published numbers for the gate set
    /// it was actually lowered to -- using `TrappedIon`'s
    /// `quantinuum_helios_2026()` figures against an `IbmQ` gate count
    /// would silently mix hardware that was never benchmarked together.
    pub fn calibration(self) -> crate::fidelity::PublishedCalibration {
        match self {
            Backend::TrappedIon => crate::fidelity::PublishedCalibration::quantinuum_helios_2026(),
            Backend::IbmQ => crate::fidelity::PublishedCalibration::ibm_heron_r2(),
            Backend::Rigetti => crate::fidelity::PublishedCalibration::rigetti_ankaa3(),
        }
    }

    /// The physical qubit connectivity `lower` routes against before
    /// doing anything else. `None` for `TrappedIon` -- a trapped-ion
    /// chain's shared motional mode makes every qubit pair directly
    /// reachable, so there's nothing to route (see `coupling.rs`'s
    /// module doc). `IbmQ`/`Rigetti` both get a nearest-neighbor chain,
    /// a deliberately conservative stand-in for their real (more
    /// permissive) lattices -- see `coupling.rs` for why a line is safe
    /// to route against even though it's not either device's literal
    /// topology.
    pub fn coupling_map(self, num_qubits: usize) -> Option<crate::coupling::CouplingMap> {
        match self {
            Backend::TrappedIon => None,
            Backend::IbmQ | Backend::Rigetti => {
                Some(crate::coupling::CouplingMap::linear(num_qubits))
            }
        }
    }
}

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
                BackendGate::Rot(q, a) => match bc.backend {
                    Backend::TrappedIon => reg.apply_ry(q, a).unwrap(),
                    Backend::IbmQ | Backend::Rigetti => reg.apply_rx(q, a).unwrap(),
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