//! The open extension point for adding a new backend to this crate.
//!
//! Before this module, `backend.rs` had a single closed
//! `enum Backend { TrappedIon, IbmQ, Rigetti }`, and every piece of
//! per-backend behavior (`lower`'s gate expansion, `push_rzz`'s gate
//! identities, `resynthesize`'s ZYZ axis shift, `Backend::calibration`,
//! `Backend::coupling_map`) was a `match backend { ... }` with one arm
//! per variant -- plus two more such matches outside `backend.rs`
//! entirely (`emit.rs`'s `apply_backend_to*`, `diagram.rs`'s
//! `from_backend`). Adding a fourth backend meant finding and correctly
//! extending every one of those matches, several of which encode
//! hand-derived exact gate identities (see `backend.rs`'s module doc)
//! that are easy to get subtly wrong by pattern-matching an existing
//! arm instead of re-deriving the underlying physics.
//!
//! This module replaces that with [`BackendSpec`], an object-safe
//! trait each backend implements once, in its own file
//! (`backend/trapped_ion.rs`, `backend/ibmq.rs`, `backend/rigetti.rs`,
//! `backend/google.rs`),
//! plus [`Backend`] -- a small `Copy` handle wrapping a
//! `&'static dyn BackendSpec` that stands in for the old enum
//! everywhere it was used *by value* (equality checks, `match
//! backend.coupling_map(...)`, storing which backend a `BackendCircuit`
//! was lowered for). Adding a new backend is then: implement
//! `BackendSpec` in a new file, add one `Backend::` constant pointing at
//! it. No existing file needs to change.
//!
//! # What stays closed on purpose: [`RotAxis`]
//! `BackendSpec::rot_axis` returns a [`RotAxis`], not a free-form value
//! -- this crate's whole native-gate story (`native.rs`'s ZYZ synthesis,
//! `emit.rs`'s execution, `diagram.rs`'s labels) is built around
//! "single-qubit gates are a `{Rz, one other axis}` Euler decomposition
//! ", and `Ry`/`Rx` are the only two axes any backend here has ever
//! needed. A backend whose native single-qubit gate genuinely isn't
//! expressible that way (or whose native two-qubit gate isn't a
//! re-expression of `Rzz`, see `push_two_qubit_zz`) doesn't fit this
//! trait's shape at all -- same boundary `backend.rs`'s module doc
//! already draws around Pasqal, extended to cover a future photonic
//! backend too. That's a real architectural limit, not an oversight:
//! widening it would mean generalizing `BackendGate` itself, which is a
//! larger, separate change from the one this module makes.

use crate::backend::BackendCircuit;
use crate::coupling::CouplingMap;
use crate::fidelity::PublishedCalibration;

/// Which axis a backend's native continuously-variable single-qubit
/// rotation (`BackendGate::Rot`) is about. See this module's doc
/// comment for why this is a closed, physically-grounded pair rather
/// than open the way [`Backend`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotAxis {
    Ry,
    Rx,
}

/// Which fixed physical interaction a `Channel::Control` pulse on this
/// backend actually represents -- needed to invert a compiled
/// [`crate::sequencer::Program`] back to the gate action it came from
/// (see [`crate::sequencer::execute`]), since
/// [`crate::pulse::PulseCalibration`]'s `two_qubit` table doesn't
/// itself distinguish `Cx` from `Cz` -- both use the exact same fixed
/// pulse shape in this crate's calibration model (see `pulse.rs`'s
/// `push_leaf_pulse`, whose `Cx`/`Cz` match arm is shared). A required
/// method, not a default, deliberately: a new `BackendSpec`
/// implementation has to say which shape its native two-qubit
/// interaction actually has rather than silently inheriting a wrong
/// guess -- the same "no convenient default that could be wrong"
/// judgment call [`BackendSpec::push_two_qubit_zz`] already makes by
/// having no default at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTwoQubitGate {
    /// Continuously parameterized -- `TrappedIon`'s `Rzz`. A
    /// `Channel::Control` pulse's amplitude directly encodes the
    /// rotation angle (see this backend's `push_two_qubit_zz`).
    ContinuousRzz,
    /// A fixed-angle native `Cx` -- `IbmQ`. Every `Channel::Control`
    /// pulse this backend emits is the same fixed gate, regardless of
    /// amplitude (the amplitude field is present but uninformative --
    /// see `pulse.rs`'s `push_leaf_pulse`).
    FixedCx,
    /// A fixed-angle native `Cz` -- `Rigetti`/`Google`.
    FixedCz,
}

/// One physical backend's native gate set, connectivity, and
/// calibration data. Implement this once per backend, in its own file,
/// to add a new target for [`crate::backend::lower`] -- see this
/// module's doc comment for the sites this replaces.
///
/// Object-safe by construction: every method takes `&self` and returns
/// owned/`Copy` data or mutates a caller-supplied `&mut BackendCircuit`,
/// so `Backend` can hold this behind a `&'static dyn BackendSpec`.
pub trait BackendSpec: Send + Sync {
    /// Stable identifier. Used for [`Backend`]'s `Debug` and
    /// `PartialEq` (trait objects have no structural equality of their
    /// own to derive from), so this must be unique across every
    /// implementing type. By convention this is the backend's old
    /// enum-variant name (`"TrappedIon"`, `"IbmQ"`, `"Rigetti"`) --
    /// some error messages (see `pulse.rs`'s backend-mismatch check)
    /// are built from `Backend`'s `Debug` output and worded around
    /// that convention, so a new backend should follow it too rather
    /// than picking an unrelated style.
    fn id(&self) -> &'static str;

    /// The published hardware-calibration figures for this backend's
    /// modeled device, for
    /// [`crate::fidelity::estimate_backend_circuit_fidelity`].
    fn calibration(&self) -> PublishedCalibration;

    /// The physical qubit connectivity [`crate::backend::lower`] routes
    /// against before lowering (see `route.rs`). `None` means no
    /// routing is needed -- e.g. a trapped-ion chain's shared motional
    /// mode reaches every qubit pair directly, so every two-qubit gate
    /// is already "adjacent" in the sense routing cares about.
    fn coupling_map(&self, num_qubits: usize) -> Option<CouplingMap>;

    /// Which axis this backend's native `BackendGate::Rot` rotates
    /// about. Drives `emit`'s and `diagram`'s interpretation of `Rot`,
    /// and `backend::resynthesize`'s ZYZ axis shift.
    fn rot_axis(&self) -> RotAxis;

    /// Pushes this backend's own encoding of the canonical two-qubit
    /// entangler `Rzz(a, b, theta)` -- the only two-qubit gate
    /// `native::decompose` ever emits -- onto `bc`. This is the one
    /// piece of physics that's genuinely backend-specific and can't be
    /// derived generically from `rot_axis` alone; see each backend's
    /// own file for the identity it uses (mirroring the derivations
    /// `backend.rs`'s module doc used to describe for `IbmQ`/`Rigetti`
    /// inline).
    fn push_two_qubit_zz(&self, bc: &mut BackendCircuit, a: usize, b: usize, theta: f64);

    /// True if this backend's native two-qubit gate set can implement a
    /// source-level `Gate::Cx` directly, without going through the
    /// generic `Rzz` canonical form (`native::decompose`'s own
    /// `H . Rzz . H` expansion of `Cx`, then this backend's own
    /// `push_two_qubit_zz`'s re-expression of *that* `Rzz`). That
    /// round-trip is always exact but not always minimal: on a backend
    /// whose native two-qubit gate already is `Cx` (`IbmQ`) or one
    /// `H`-sandwich away from it (`Rigetti`/`Google`'s `Cz`), it costs
    /// 2 native two-qubit gates to implement something that's really 1
    /// -- and since a routing-inserted `Gate::Swap` is
    /// `Cx(a,b).Cx(b,a).Cx(a,b)` at the IR level (see `route.rs`), the
    /// same 2x tax applies to every SWAP a routed circuit needed, which
    /// is usually the larger share of it. Defaults to `false`, the
    /// always-correct choice for a new backend that hasn't opted in --
    /// see [`push_native_cx`](Self::push_native_cx) for the
    /// corresponding gate-pushing method, only ever called when this is
    /// `true`. Must return the same answer regardless of which qubits
    /// are involved; `backend::lower` checks it once per circuit
    /// lowering, not per gate.
    fn has_native_cx(&self) -> bool {
        false
    }

    /// Pushes this backend's native-gate implementation of
    /// `Gate::Cx(control, target)` onto `bc` -- see
    /// [`has_native_cx`](Self::has_native_cx)'s doc comment for why
    /// this exists and when it's worth implementing. Only ever called
    /// by `backend::lower` when `has_native_cx()` returns `true`; the
    /// default implementation is unreachable for any backend that
    /// hasn't overridden that method, since nothing else in this crate
    /// calls this directly.
    fn push_native_cx(&self, bc: &mut BackendCircuit, control: usize, target: usize) {
        let _ = (bc, control, target);
        unreachable!(
            "push_native_cx called on a BackendSpec whose has_native_cx() is false -- \
             backend::lower should never do this"
        )
    }

    /// True only for a backend whose native gate set already *is*
    /// `native::decompose`'s canonical `{Rz, Ry-as-Rot, Rzz}` form, so
    /// [`crate::backend::lower`] can skip the generic per-gate
    /// re-expansion + resynthesize/optimize fixed-point loop entirely
    /// and just relabel `native::decompose`'s own output. `TrappedIon`
    /// is the only backend where this holds today; every other backend
    /// re-expresses at least one of `{Ry, Rzz}` in different native
    /// gates and needs the general path. Defaults to `false`, the safe
    /// choice for any new backend: the general path is always exact
    /// (it's built entirely from exact circuit identities), just
    /// potentially not the minimal-gate-count route for a backend that
    /// happens to need no re-expression at all.
    fn is_native_decompose_target(&self) -> bool {
        false
    }

    /// Which fixed physical interaction this backend's `Channel::Control`
    /// pulses represent -- see [`NativeTwoQubitGate`]'s doc comment.
    /// No default: every backend must say which of the three shapes
    /// applies, since guessing wrong here would make
    /// [`crate::sequencer::execute`] apply the wrong two-qubit gate
    /// silently rather than erroring.
    fn native_two_qubit_gate(&self) -> NativeTwoQubitGate;
}

/// A handle to one backend's [`BackendSpec`] implementation. `Copy`,
/// comparable, and storable on a [`BackendCircuit`] the same way the
/// old `enum Backend` was -- see this module's doc comment for what
/// changed underneath.
#[derive(Clone, Copy)]
pub struct Backend(&'static dyn BackendSpec);

impl Backend {
    /// Quantinuum-Helios-style trapped-ion target: `{Rz, Ry, Rzz}`.
    #[allow(non_upper_case_globals)]
    pub const TrappedIon: Backend = Backend(&crate::backend::trapped_ion::TrappedIonSpec);
    /// IBM-superconducting-style target: `{Rz, Rx, Cx}`.
    #[allow(non_upper_case_globals)]
    pub const IbmQ: Backend = Backend(&crate::backend::ibmq::IbmQSpec);
    /// Rigetti-superconducting-style target: `{Rz, Rx, Cz}`.
    #[allow(non_upper_case_globals)]
    pub const Rigetti: Backend = Backend(&crate::backend::rigetti::RigettiSpec);
    /// Google-superconducting-style target: `{Rz, Rx, Cz}` (Willow,
    /// CZ-tuned configuration).
    #[allow(non_upper_case_globals)]
    pub const Google: Backend = Backend(&crate::backend::google::GoogleSpec);

    /// Wraps an arbitrary [`BackendSpec`] implementation as a
    /// [`Backend`] handle, for a backend that -- unlike the three
    /// built-in constants above -- isn't wired into this crate as a
    /// named constant (e.g. one defined and registered by a downstream
    /// crate). Most callers want [`Backend::TrappedIon`]/`IbmQ`/
    /// `Rigetti` instead.
    pub const fn from_spec(spec: &'static dyn BackendSpec) -> Backend {
        Backend(spec)
    }

    pub fn id(self) -> &'static str {
        self.0.id()
    }
    pub fn calibration(self) -> PublishedCalibration {
        self.0.calibration()
    }
    pub fn coupling_map(self, num_qubits: usize) -> Option<CouplingMap> {
        self.0.coupling_map(num_qubits)
    }
    pub fn rot_axis(self) -> RotAxis {
        self.0.rot_axis()
    }
    pub(crate) fn push_two_qubit_zz(self, bc: &mut BackendCircuit, a: usize, b: usize, theta: f64) {
        self.0.push_two_qubit_zz(bc, a, b, theta)
    }
    pub(crate) fn has_native_cx(self) -> bool {
        self.0.has_native_cx()
    }
    pub(crate) fn push_native_cx(self, bc: &mut BackendCircuit, control: usize, target: usize) {
        self.0.push_native_cx(bc, control, target)
    }
    pub(crate) fn is_native_decompose_target(self) -> bool {
        self.0.is_native_decompose_target()
    }
    pub fn native_two_qubit_gate(self) -> NativeTwoQubitGate {
        self.0.native_two_qubit_gate()
    }
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.id())
    }
}

impl PartialEq for Backend {
    fn eq(&self, other: &Self) -> bool {
        self.0.id() == other.0.id()
    }
}
impl Eq for Backend {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_backends_have_distinct_ids() {
        let ids = [
            Backend::TrappedIon.id(),
            Backend::IbmQ.id(),
            Backend::Rigetti.id(),
            Backend::Google.id(),
        ];
        for i in 0..ids.len() {
            for j in 0..ids.len() {
                if i != j {
                    assert_ne!(ids[i], ids[j], "backend ids must be unique: {:?}", ids);
                }
            }
        }
    }

    #[test]
    fn backend_equality_is_by_identity_not_just_axis() {
        // IbmQ and Rigetti share RotAxis::Rx but must not compare equal.
        assert_eq!(Backend::IbmQ.rot_axis(), Backend::Rigetti.rot_axis());
        assert_ne!(Backend::IbmQ, Backend::Rigetti);
        assert_eq!(Backend::IbmQ, Backend::IbmQ);
    }

    #[test]
    fn google_and_rigetti_are_distinct_despite_sharing_axis_and_gate_shape() {
        // Google (Willow, CZ-tuned) and Rigetti both use RotAxis::Rx
        // and structurally the same push_two_qubit_zz identity (see
        // backend/google.rs's doc comment) -- confirms that sharing an
        // identity doesn't collapse two backends into "the same" one.
        assert_eq!(Backend::Google.rot_axis(), Backend::Rigetti.rot_axis());
        assert_ne!(Backend::Google, Backend::Rigetti);
        assert_ne!(Backend::Google.id(), Backend::Rigetti.id());
    }
}