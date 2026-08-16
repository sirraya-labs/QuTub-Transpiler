//! Source intermediate representation: the gate set a QASM 2.0 program
//! (or any other frontend) is parsed into, before native-gate
//! decomposition. This is deliberately a *rich* gate set (mirrors
//! `sirraya_qutub::core::QuantumRegister`'s `apply_*` surface) -- the
//! narrowing to a hardware-native set happens in [`crate::native`].

/// A qubit as addressed by the *input program*: `q0`, `q1`, ... exactly
/// as declared in a `qreg` (or however a non-QASM frontend numbers its
/// qubits). Logical identity never changes -- `route::route` moves a
/// logical qubit's *physical* location, never renumbers the logical
/// qubit itself (see [`PhysicalQubit`] and `route.rs`'s module doc).
///
/// Deliberately a thin, zero-cost newtype (`#[repr(transparent)]`-
/// equivalent) rather than a richer type: the only thing this needs to
/// guarantee is that a logical index and a physical index can't be
/// passed to each other's slot by accident, not that either carries
/// any other invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogicalQubit(pub usize);

/// A qubit as addressed by *hardware wire position* after routing: a
/// row in a [`crate::coupling::CouplingMap`]. Which logical qubit's
/// state currently lives on a given physical qubit changes over the
/// course of a circuit as `route::route` inserts `Swap`s -- see
/// `route.rs`'s module doc for the full logical/physical mapping
/// story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PhysicalQubit(pub usize);

impl From<usize> for LogicalQubit {
    fn from(q: usize) -> Self {
        LogicalQubit(q)
    }
}
impl From<usize> for PhysicalQubit {
    fn from(q: usize) -> Self {
        PhysicalQubit(q)
    }
}
impl std::fmt::Display for LogicalQubit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "q{}", self.0)
    }
}
impl std::fmt::Display for PhysicalQubit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "p{}", self.0)
    }
}

// NOTE on scope: `Gate`'s own qubit fields below are deliberately left
// as plain `usize`, not `LogicalQubit`/`PhysicalQubit`, in this pass.
// `Gate`/`Circuit` are reused as-is both *before* routing (qubit
// fields mean logical indices) and *after* it (`route::route`'s
// output `Circuit` -- same `Gate` type -- means physical indices).
// Giving `Gate` a real logical/physical type parameter is a real,
// larger design change (effectively `Gate<Q>`/`Circuit<Q>`, touching
// every module that builds or consumes a `Circuit`) and is tracked as
// separate follow-up work rather than folded into this change. What
// *is* fixed here is the actual bug-prone spot: `route.rs`'s internal
// `logical_to_physical`/`physical_to_logical` bookkeeping, which used
// to be two parallel `Vec<usize>` that were easy to mix up, and now
// use these two distinct types instead.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    H(usize),
    X(usize),
    Y(usize),
    Z(usize),
    S(usize),
    Sdg(usize),
    T(usize),
    Tdg(usize),
    Rx(usize, f64),
    Ry(usize, f64),
    Rz(usize, f64),
    Cx(usize, usize),
    Cz(usize, usize),
    Swap(usize, usize),
    Rxx(usize, usize, f64),
    Ryy(usize, usize, f64),
    Rzz(usize, usize, f64),
    /// Controlled phase: diag(1,1,1,e^{i*lambda}).
    Cp(usize, usize, f64),
    /// Measures qubit `q` (whichever physical wire it's currently on --
    /// see `route.rs`) into classical bit `c`. Not a unitary rewrite
    /// target: `native.rs`/`backend.rs` pass it through unchanged
    /// rather than decomposing it, and it must never be treated as
    /// reorderable relative to *any* other gate by `ir_optimize.rs`'s
    /// commuting pass -- two `Measure`s that write different qubits
    /// into the *same* classical bit `c` are only disjoint by qubit,
    /// not by the classical side effect that actually matters, so
    /// `ir_optimize::disjoint` special-cases `Measure` to never commute
    /// past anything.
    Measure(usize, usize),
    /// Applies `inner` iff classical bit `clbit` currently holds
    /// `value` -- real, per-shot classical feed-forward control, not a
    /// stand-in for it. This is the piece `examples/quantum_teleportation.rs`
    /// used to work around by applying Bob's correction directly
    /// against the `QuantumRegister` after `emit::run`, specifically
    /// because this variant didn't exist yet -- see that example's own
    /// doc comment (now updated) for the before/after.
    ///
    /// `inner` is always a single, concrete gate application, and
    /// `Circuit::validate` enforces two shape restrictions on it:
    /// - **Never `Measure`.** A measurement's whole job is to *produce*
    ///   a classical bit; conditioning one on a classical bit has no
    ///   physical meaning.
    /// - **Never another `If`.** This stays a flat, one-level wrapper
    ///   rather than an arbitrarily-recursive one -- every real use
    ///   this crate has (teleportation's Bob-side correction, and any
    ///   single conditioned gate a QASM `if` statement can express) is
    ///   one condition on one gate.
    ///
    /// `native::decompose_gate` distributes the condition across every
    /// native gate `inner` decomposes to (e.g. a conditioned `H` becomes
    /// a conditioned `Rz`/`Ry`/`Rz` triple, each wrapped in its own
    /// `NativeGate::If` with the same `clbit`/`value`), so a caller
    /// never needs to build a nested `If` even for a multi-gate `inner`.
    If(usize, bool, Box<Gate>),
}

impl Gate {
    /// Qubit indices this gate touches, in the order they appear in the
    /// gate's own argument list (control(s) before target, as written).
    /// For `If`, this is simply `inner`'s own qubits -- an `If` occupies
    /// exactly the wires its conditioned gate does, nothing more, so
    /// routing (`route.rs`) and source-level reordering (`ir_optimize.rs`)
    /// both already do the right thing by calling this generically,
    /// with no `Gate::If`-specific case of their own needed for qubit
    /// placement (only for whether they're allowed to *reorder* it --
    /// see `ir_optimize::disjoint`).
    pub fn qubits(&self) -> Vec<usize> {
        use Gate::*;
        match *self {
            H(q) | X(q) | Y(q) | Z(q) | S(q) | Sdg(q) | T(q) | Tdg(q) | Rx(q, _) | Ry(q, _)
            | Rz(q, _) | Measure(q, _) => vec![q],
            Cx(a, b) | Cz(a, b) | Swap(a, b) | Rxx(a, b, _) | Ryy(a, b, _) | Rzz(a, b, _)
            | Cp(a, b, _) => vec![a, b],
            If(_, _, ref inner) => inner.qubits(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Circuit {
    pub num_qubits: usize,
    /// Number of classical bits available to `Gate::Measure`. Mirrors
    /// how `num_qubits` is set from `qreg` in `qasm.rs`: `creg` sets
    /// this the same way, instead of being parsed-and-discarded.
    pub num_clbits: usize,
    pub gates: Vec<Gate>,
}

impl Circuit {
    pub fn new(num_qubits: usize) -> Self {
        Self {
            num_qubits,
            num_clbits: 0,
            gates: Vec::new(),
        }
    }

    pub fn push(&mut self, gate: Gate) -> &mut Self {
        self.gates.push(gate);
        self
    }

    /// Counts of each source-level gate kind, keyed by a short QASM-style
    /// mnemonic. Useful for a quick before/after diff around optimization.
    pub fn gate_counts(&self) -> std::collections::BTreeMap<&'static str, usize> {
        use Gate::*;
        let mut counts = std::collections::BTreeMap::new();
        for g in &self.gates {
            let name = match g {
                H(_) => "h",
                X(_) => "x",
                Y(_) => "y",
                Z(_) => "z",
                S(_) => "s",
                Sdg(_) => "sdg",
                T(_) => "t",
                Tdg(_) => "tdg",
                Rx(..) => "rx",
                Ry(..) => "ry",
                Rz(..) => "rz",
                Cx(..) => "cx",
                Cz(..) => "cz",
                Swap(..) => "swap",
                Rxx(..) => "rxx",
                Ryy(..) => "ryy",
                Rzz(..) => "rzz",
                Cp(..) => "cp",
                Measure(..) => "measure",
                If(..) => "if",
            };
            *counts.entry(name).or_insert(0) += 1;
        }
        counts
    }

    /// Checks every IR invariant this crate currently relies on
    /// elsewhere by convention, in one place, so a `Circuit` built any
    /// way other than `qasm::parse` (which already range-checks as it
    /// goes -- see its own module doc) can still be checked before
    /// being handed to `route`/`native::decompose`/`backend::lower`,
    /// none of which re-check these themselves today.
    ///
    /// Checks, in order (returns on the first violation found, same
    /// style as `qasm::parse`'s own error reporting):
    /// 1. **Qubit references are valid** -- every qubit index any gate
    ///    touches is `< self.num_qubits`.
    /// 2. **Classical destinations are valid** -- every `Measure`'s
    ///    clbit index is `< self.num_clbits`.
    /// 3. **Two-qubit gates don't self-target** -- a two-qubit gate's
    ///    two qubit arguments must be distinct (`Cx(0, 0)` has no
    ///    physical meaning and every decomposition identity in
    ///    `native.rs`/`backend.rs` implicitly assumes its two
    ///    arguments are different wires).
    /// 4. **Gate parameters are finite** -- no angle parameter
    ///    (`Rx`/`Ry`/`Rz`/`Rxx`/`Ryy`/`Rzz`/`Cp`'s float argument) is
    ///    `NaN` or `+-inf`. `native.rs`'s ZYZ synthesis and
    ///    `optimize.rs`'s angle-merging both silently propagate a NaN
    ///    into every subsequent decision (`NaN.abs() > EPS` is always
    ///    true, so a NaN angle is never dropped, never merged
    ///    correctly, and poisons everything downstream) rather than
    ///    erroring -- so this is checked here, at construction-time
    ///    validation, instead of relying on each pass to notice.
    ///
    /// Gate *arity* (the right number of qubit arguments per kind) is
    /// deliberately not checked here: `Gate`'s own variants
    /// (`Cx(usize, usize)` vs. `H(usize)`, etc.) already make an
    /// arity mismatch a compile error, not a runtime one -- there is
    /// no way to construct an ill-formed `Gate` in the first
    /// place, so a runtime check would only ever be dead code.
    /// 5. **`If`'s own shape.** Its classical bit index is `< self.num_clbits`,
    ///    and its `inner` gate is neither `Measure` (see `Gate::If`'s
    ///    doc comment on why that has no meaning) nor another `If`
    ///    (this stays a flat, one-level wrapper). `inner` itself is
    ///    then checked against every rule above, recursively -- so a
    ///    conditioned `Cx(0, 0)` or a conditioned `NaN` angle is caught
    ///    exactly as it would be unconditioned.
    pub fn validate(&self) -> Result<(), String> {
        for (i, gate) in self.gates.iter().enumerate() {
            self.validate_gate(i, gate)?;
        }
        Ok(())
    }

    /// The per-gate half of [`validate`], factored out so it can be
    /// called recursively on the `inner` gate an `If` wraps -- see
    /// `validate`'s point 5. `i` is always the *top-level* gate index
    /// (an `If`'s `inner` isn't a separate entry in `self.gates`), so
    /// every error message -- including one about a bad `inner` -- names
    /// the `If` statement a caller actually sees in their source.
    fn validate_gate(&self, i: usize, gate: &Gate) -> Result<(), String> {
        for q in gate.qubits() {
            if q >= self.num_qubits {
                return Err(format!(
                    "gate {} ({:?}): qubit index {} out of range for {} qubit(s)",
                    i, gate, q, self.num_qubits
                ));
            }
        }
        if let Gate::Measure(_, c) = *gate {
            if c >= self.num_clbits {
                return Err(format!(
                    "gate {} ({:?}): classical bit index {} out of range for {} clbit(s)",
                    i, gate, c, self.num_clbits
                ));
            }
        }
        let qs = gate.qubits();
        if qs.len() == 2 && qs[0] == qs[1] {
            return Err(format!(
                "gate {} ({:?}): a two-qubit gate's arguments must be distinct, both are {}",
                i, gate, qs[0]
            ));
        }
        let angle = match *gate {
            Gate::Rx(_, a) | Gate::Ry(_, a) | Gate::Rz(_, a) => Some(a),
            Gate::Rxx(_, _, a) | Gate::Ryy(_, _, a) | Gate::Rzz(_, _, a) => Some(a),
            Gate::Cp(_, _, a) => Some(a),
            _ => None,
        };
        if let Some(a) = angle {
            if !a.is_finite() {
                return Err(format!(
                    "gate {} ({:?}): parameter {} is not finite (NaN or infinite)",
                    i, gate, a
                ));
            }
        }
        if let Gate::If(clbit, _, ref inner) = *gate {
            if clbit >= self.num_clbits {
                return Err(format!(
                    "gate {} ({:?}): If's classical bit index {} out of range for {} clbit(s)",
                    i, gate, clbit, self.num_clbits
                ));
            }
            match inner.as_ref() {
                Gate::Measure(..) => {
                    return Err(format!(
                        "gate {} ({:?}): If cannot condition a Measure -- a measurement \
                         produces a classical bit, it doesn't consume one",
                        i, gate
                    ));
                }
                Gate::If(..) => {
                    return Err(format!(
                        "gate {} ({:?}): If cannot condition another If -- nested classical \
                         conditions aren't supported here, flatten to a single condition",
                        i, gate
                    ));
                }
                _ => self.validate_gate(i, inner)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_circuit_is_valid() {
        let c = Circuit::new(3);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn well_formed_circuit_is_valid() {
        let mut c = Circuit::new(2);
        c.num_clbits = 2;
        c.push(Gate::H(0)).push(Gate::Cx(0, 1)).push(Gate::Measure(0, 0)).push(Gate::Measure(1, 1));
        assert!(c.validate().is_ok());
    }

    #[test]
    fn rejects_qubit_index_out_of_range_on_a_single_qubit_gate() {
        let mut c = Circuit::new(2);
        c.push(Gate::H(5));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_qubit_index_out_of_range_on_a_two_qubit_gate() {
        let mut c = Circuit::new(2);
        c.push(Gate::Cx(0, 9));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_clbit_index_out_of_range_for_measure() {
        let mut c = Circuit::new(2);
        c.num_clbits = 1;
        c.push(Gate::Measure(0, 7));
        assert!(c.validate().is_err());
    }

    #[test]
    fn accepts_measure_with_in_range_clbit_even_if_num_clbits_was_never_set_explicitly() {
        // num_clbits defaults to 0 (see Circuit::new), so a Measure
        // pushed without first bumping num_clbits should be rejected,
        // not silently accepted -- this is the mirror case of the
        // rejects_ test above, confirming the check isn't vacuously
        // true for the default-zero case.
        let mut c = Circuit::new(1);
        c.push(Gate::Measure(0, 0));
        assert!(c.validate().is_err(), "num_clbits is still 0, clbit 0 is out of range");
    }

    #[test]
    fn rejects_two_qubit_gate_with_identical_arguments() {
        let mut c = Circuit::new(2);
        c.push(Gate::Cx(0, 0));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_swap_with_identical_arguments() {
        let mut c = Circuit::new(2);
        c.push(Gate::Swap(1, 1));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_nan_angle() {
        let mut c = Circuit::new(1);
        c.push(Gate::Rz(0, f64::NAN));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_infinite_angle() {
        let mut c = Circuit::new(2);
        c.push(Gate::Rzz(0, 1, f64::INFINITY));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_infinite_cp_lambda() {
        let mut c = Circuit::new(2);
        c.push(Gate::Cp(0, 1, f64::NEG_INFINITY));
        assert!(c.validate().is_err());
    }

    #[test]
    fn accepts_zero_and_negative_finite_angles() {
        let mut c = Circuit::new(1);
        c.push(Gate::Rx(0, 0.0)).push(Gate::Ry(0, -3.7));
        assert!(c.validate().is_ok());
    }

    #[test]
    fn error_message_identifies_the_offending_gate_index() {
        let mut c = Circuit::new(2);
        c.push(Gate::H(0)).push(Gate::H(1)).push(Gate::Cx(0, 12));
        let err = c.validate().unwrap_err();
        assert!(err.contains("gate 2"), "error should name the offending gate's index: {}", err);
    }

    #[test]
    fn gate_qubits_matches_expected_arity() {
        // Sanity check on the qubits() helper validate() itself relies
        // on, for both shapes it distinguishes.
        assert_eq!(Gate::H(3).qubits(), vec![3]);
        assert_eq!(Gate::Measure(2, 0).qubits(), vec![2]);
        assert_eq!(Gate::Cx(0, 1).qubits(), vec![0, 1]);
        assert_eq!(Gate::Rzz(4, 5, 0.1).qubits(), vec![4, 5]);
    }

    #[test]
    fn if_qubits_delegates_to_inner() {
        // A single-qubit inner...
        assert_eq!(Gate::If(0, true, Box::new(Gate::X(2))).qubits(), vec![2]);
        // ...and a two-qubit inner, unchanged from what `route.rs`/
        // `ir_optimize.rs` need to place/reorder it correctly with no
        // `Gate::If`-specific case of their own (see `qubits`'s doc
        // comment).
        assert_eq!(Gate::If(0, true, Box::new(Gate::Cx(1, 3))).qubits(), vec![1, 3]);
    }

    #[test]
    fn accepts_well_formed_if() {
        let mut c = Circuit::new(2);
        c.num_clbits = 1;
        c.push(Gate::Measure(0, 0)).push(Gate::If(0, true, Box::new(Gate::X(1))));
        assert!(c.validate().is_ok());
    }

    #[test]
    fn rejects_if_clbit_out_of_range() {
        let mut c = Circuit::new(2);
        c.num_clbits = 1;
        c.push(Gate::If(5, true, Box::new(Gate::X(1))));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_if_conditioning_a_measure() {
        let mut c = Circuit::new(2);
        c.num_clbits = 1;
        c.push(Gate::If(0, true, Box::new(Gate::Measure(1, 0))));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_nested_if() {
        let mut c = Circuit::new(2);
        c.num_clbits = 1;
        c.push(Gate::If(0, true, Box::new(Gate::If(0, false, Box::new(Gate::X(1))))));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_if_wrapping_a_qubit_out_of_range() {
        // The inner gate's own qubit-range check must still fire
        // through the wrapper.
        let mut c = Circuit::new(2);
        c.num_clbits = 1;
        c.push(Gate::If(0, true, Box::new(Gate::X(9))));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_if_wrapping_a_nan_angle() {
        let mut c = Circuit::new(1);
        c.num_clbits = 1;
        c.push(Gate::If(0, true, Box::new(Gate::Rz(0, f64::NAN))));
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_if_wrapping_a_self_targeting_two_qubit_gate() {
        let mut c = Circuit::new(2);
        c.num_clbits = 1;
        c.push(Gate::If(0, true, Box::new(Gate::Cx(0, 0))));
        assert!(c.validate().is_err());
    }

    #[test]
    fn if_counts_under_its_own_mnemonic() {
        let mut c = Circuit::new(1);
        c.num_clbits = 1;
        c.push(Gate::If(0, true, Box::new(Gate::X(0))));
        assert_eq!(c.gate_counts().get("if"), Some(&1));
    }
}