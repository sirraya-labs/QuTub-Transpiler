//! Decomposition into a trapped-ion-style native gate set: arbitrary
//! single-qubit rotations built from `{Rz, Ry}` (Euler ZYZ form) plus a
//! single two-qubit entangler, `Rzz` -- `sirraya_qutub`'s
//! `apply_rz` / `apply_ry` / `apply_rzz`. This is the gate set the
//! crate's `HardwareCalibration` (Quantinuum Helios, a trapped-ion
//! device) story is actually about: one single-qubit fidelity number and
//! one two-qubit fidelity number, which only makes sense once every gate
//! in the circuit *is* one of those two kinds.
//!
//! Every identity below is an exact (not approximate) circuit identity;
//! `tests/decompositions.rs` checks each one against
//! `sirraya_qutub::core::QuantumRegister` directly rather than trusting
//! the algebra alone.

use crate::ir::{Circuit, Gate};
use std::f64::consts::{FRAC_PI_2, PI};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NativeGate {
    Rz(usize, f64),
    Ry(usize, f64),
    Rzz(usize, usize, f64),
    /// Passed through unchanged from `ir::Gate::Measure` -- not a
    /// unitary rewrite target, so it never appears on the left-hand
    /// side of any decomposition identity in this module.
    Measure(usize, usize),
}

#[derive(Debug, Clone, Default)]
pub struct NativeCircuit {
    pub num_qubits: usize,
    pub num_clbits: usize,
    pub gates: Vec<NativeGate>,
}

impl NativeCircuit {
    pub fn new(num_qubits: usize) -> Self {
        Self {
            num_qubits,
            num_clbits: 0,
            gates: Vec::new(),
        }
    }

    pub fn push(&mut self, g: NativeGate) {
        self.gates.push(g);
    }

    pub fn extend(&mut self, other: impl IntoIterator<Item = NativeGate>) {
        self.gates.extend(other);
    }

    /// (single_qubit_gate_count, two_qubit_gate_count) -- exactly the two
    /// numbers `HardwareCalibration`'s fidelity story needs.
    ///
    /// `Measure` is deliberately excluded from both counts: it isn't a
    /// unitary gate, so a per-gate depolarizing-error model has nothing
    /// to say about it, and counting it as a "single-qubit gate" here
    /// would silently make `fidelity::estimate_circuit_fidelity` price
    /// a measurement as if it were a rotation.
    pub fn gate_counts(&self) -> (usize, usize) {
        let mut single = 0;
        let mut two = 0;
        for g in &self.gates {
            match g {
                NativeGate::Rz(..) | NativeGate::Ry(..) => single += 1,
                NativeGate::Rzz(..) => two += 1,
                NativeGate::Measure(..) => {}
            }
        }
        (single, two)
    }
}

// ---------------------------------------------------------------------
// Minimal local complex/matrix algebra, kept private to this module so
// the ZYZ synthesizer below doesn't need to reach into
// `sirraya_qutub::complex::Complex`'s representation -- it only needs
// to reproduce the *external* behavior of qutub's own Rz/Ry/named gates,
// which is documented (and tested) independently of how qutub happens
// to store a complex number internally.
// ---------------------------------------------------------------------

// `C`/`Mat2` and the handful of matrix builders below are `pub(crate)`
// (rather than private to this module) so `crate::backend`'s
// full-run resynthesis pass can reuse the *exact same* ZYZ algebra
// this module already has tested against the real simulator, instead
// of re-deriving a second copy of the same math. Everything downstream
// still only ever touches these through the opaque `Mat2` type and the
// named constructor/decompose functions -- no field access, no way to
// build an invalid `C`/`Mat2` from outside this module.
#[derive(Clone, Copy, Debug)]
pub(crate) struct C {
    re: f64,
    im: f64,
}

impl C {
    fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }
    fn polar(r: f64, theta: f64) -> Self {
        Self::new(r * theta.cos(), r * theta.sin())
    }
    fn abs(self) -> f64 {
        (self.re * self.re + self.im * self.im).sqrt()
    }
    fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
}
impl std::ops::Mul for C {
    type Output = C;
    fn mul(self, o: C) -> C {
        C::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}
impl std::ops::Sub for C {
    type Output = C;
    fn sub(self, o: C) -> C {
        C::new(self.re - o.re, self.im - o.im)
    }
}
impl std::ops::Add for C {
    type Output = C;
    fn add(self, o: C) -> C {
        C::new(self.re + o.re, self.im + o.im)
    }
}

pub(crate) type Mat2 = [[C; 2]; 2];

const EPS: f64 = 1e-9;

pub(crate) fn m_identity() -> Mat2 {
    [
        [C::new(1.0, 0.0), C::new(0.0, 0.0)],
        [C::new(0.0, 0.0), C::new(1.0, 0.0)],
    ]
}

/// 2x2 matrix product `a . b` (`b` applied first, `a` last -- same
/// "rightmost factor is applied first" convention `zyz_decompose`'s
/// doc comment uses).
pub(crate) fn matmul(a: Mat2, b: Mat2) -> Mat2 {
    let mut out = m_identity();
    for i in 0..2 {
        for j in 0..2 {
            out[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j];
        }
    }
    out
}

pub(crate) fn m_rz(theta: f64) -> Mat2 {
    let h = theta / 2.0;
    [
        [C::polar(1.0, -h), C::new(0.0, 0.0)],
        [C::new(0.0, 0.0), C::polar(1.0, h)],
    ]
}

pub(crate) fn m_ry(theta: f64) -> Mat2 {
    let (c, s) = ((theta / 2.0).cos(), (theta / 2.0).sin());
    [
        [C::new(c, 0.0), C::new(-s, 0.0)],
        [C::new(s, 0.0), C::new(c, 0.0)],
    ]
}

/// ZYZ Euler decomposition: for any single-qubit unitary `m` (as written
/// in the *same* matrix convention as `sirraya_qutub`'s own gates --
/// see the module doc), returns `(delta, gamma, beta)` such that
/// `Rz(beta) . Ry(gamma) . Rz(delta) == m` up to an unobservable global
/// phase. `delta` is applied to the qubit first, `beta` last.
pub(crate) fn zyz_decompose(m: Mat2) -> (f64, f64, f64) {
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    let g = C::polar(1.0, -det.arg() / 2.0); // det(g*m) == 1
    let v00 = g * m[0][0];
    let v01 = g * m[0][1];

    let c = v00.abs();
    let s = v01.abs();
    let gamma = 2.0 * s.atan2(c);

    // v00 = cos(gamma/2) * e^{-i(beta+delta)/2}
    // v01 = -sin(gamma/2) * e^{-i(beta-delta)/2}
    if c < EPS {
        let beta_minus_delta = -2.0 * (v01 * C::new(-1.0, 0.0)).arg();
        (0.0, gamma, beta_minus_delta)
    } else if s < EPS {
        let beta_plus_delta = -2.0 * v00.arg();
        (0.0, gamma, beta_plus_delta)
    } else {
        let beta_plus_delta = -2.0 * v00.arg();
        let beta_minus_delta = -2.0 * (v01 * C::new(-1.0, 0.0)).arg();
        let beta = 0.5 * (beta_plus_delta + beta_minus_delta);
        let delta = 0.5 * (beta_plus_delta - beta_minus_delta);
        (delta, gamma, beta)
    }
}

fn push_single(nc: &mut NativeCircuit, q: usize, m: Mat2) {
    let (delta, gamma, beta) = zyz_decompose(m);
    if delta.abs() > EPS {
        nc.push(NativeGate::Rz(q, delta));
    }
    if gamma.abs() > EPS {
        nc.push(NativeGate::Ry(q, gamma));
    }
    if beta.abs() > EPS {
        nc.push(NativeGate::Rz(q, beta));
    }
}

fn m_h() -> Mat2 {
    let f = 1.0 / std::f64::consts::SQRT_2;
    [[C::new(f, 0.0), C::new(f, 0.0)], [C::new(f, 0.0), C::new(-f, 0.0)]]
}
fn m_x() -> Mat2 {
    [[C::new(0.0, 0.0), C::new(1.0, 0.0)], [C::new(1.0, 0.0), C::new(0.0, 0.0)]]
}
fn m_y() -> Mat2 {
    [[C::new(0.0, 0.0), C::new(0.0, -1.0)], [C::new(0.0, 1.0), C::new(0.0, 0.0)]]
}
fn m_z() -> Mat2 {
    [[C::new(1.0, 0.0), C::new(0.0, 0.0)], [C::new(0.0, 0.0), C::new(-1.0, 0.0)]]
}
fn m_s() -> Mat2 {
    [[C::new(1.0, 0.0), C::new(0.0, 0.0)], [C::new(0.0, 0.0), C::new(0.0, 1.0)]]
}
fn m_sdg() -> Mat2 {
    [[C::new(1.0, 0.0), C::new(0.0, 0.0)], [C::new(0.0, 0.0), C::new(0.0, -1.0)]]
}
fn m_t() -> Mat2 {
    [
        [C::new(1.0, 0.0), C::new(0.0, 0.0)],
        [C::new(0.0, 0.0), C::polar(1.0, FRAC_PI_2 / 2.0)],
    ]
}
fn m_tdg() -> Mat2 {
    [
        [C::new(1.0, 0.0), C::new(0.0, 0.0)],
        [C::new(0.0, 0.0), C::polar(1.0, -FRAC_PI_2 / 2.0)],
    ]
}
pub(crate) fn m_rx(theta: f64) -> Mat2 {
    let (c, s) = ((theta / 2.0).cos(), (theta / 2.0).sin());
    [[C::new(c, 0.0), C::new(0.0, -s)], [C::new(0.0, -s), C::new(c, 0.0)]]
}

/// Decomposes a full source-level [`Circuit`] into the native
/// `{Rz, Ry, Rzz}` gate set.
pub fn decompose(circuit: &Circuit) -> NativeCircuit {
    let mut nc = NativeCircuit::new(circuit.num_qubits);
    nc.num_clbits = circuit.num_clbits;
    for gate in &circuit.gates {
        decompose_gate(&mut nc, gate);
    }
    nc
}

fn decompose_gate(nc: &mut NativeCircuit, gate: &Gate) {
    match *gate {
        Gate::Measure(q, c) => nc.push(NativeGate::Measure(q, c)),
        Gate::H(q) => push_single(nc, q, m_h()),
        Gate::X(q) => push_single(nc, q, m_x()),
        Gate::Y(q) => push_single(nc, q, m_y()),
        Gate::Z(q) => push_single(nc, q, m_z()),
        Gate::S(q) => push_single(nc, q, m_s()),
        Gate::Sdg(q) => push_single(nc, q, m_sdg()),
        Gate::T(q) => push_single(nc, q, m_t()),
        Gate::Tdg(q) => push_single(nc, q, m_tdg()),
        Gate::Rx(q, theta) => push_single(nc, q, m_rx(theta)),
        Gate::Ry(q, theta) => {
            if theta.abs() > EPS {
                nc.push(NativeGate::Ry(q, theta));
            }
        }
        Gate::Rz(q, theta) => {
            if theta.abs() > EPS {
                nc.push(NativeGate::Rz(q, theta));
            }
        }
        Gate::Rzz(a, b, theta) => {
            if theta.abs() > EPS {
                nc.push(NativeGate::Rzz(a, b, theta));
            }
        }
        Gate::Cp(a, b, lambda) => decompose_cp(nc, a, b, lambda),
        Gate::Cz(a, b) => decompose_cp(nc, a, b, PI),
        Gate::Cx(control, target) => {
            push_single(nc, target, m_h());
            decompose_cp(nc, control, target, PI);
            push_single(nc, target, m_h());
        }
        Gate::Swap(a, b) => {
            decompose_gate(nc, &Gate::Cx(a, b));
            decompose_gate(nc, &Gate::Cx(b, a));
            decompose_gate(nc, &Gate::Cx(a, b));
        }
        Gate::Rxx(a, b, theta) => {
            // RXX(theta) = (H@H) . RZZ(theta) . (H@H), exact: X = H Z H.
            push_single(nc, a, m_h());
            push_single(nc, b, m_h());
            if theta.abs() > EPS {
                nc.push(NativeGate::Rzz(a, b, theta));
            }
            push_single(nc, a, m_h());
            push_single(nc, b, m_h());
        }
        Gate::Ryy(a, b, theta) => {
            // RYY(theta) = (Rx(-pi/2)@Rx(-pi/2)) . RZZ(theta) . (Rx(pi/2)@Rx(pi/2)),
            // exact: Y = Rx(-pi/2) . Z . Rx(-pi/2)^dagger.
            push_single(nc, a, m_rx(FRAC_PI_2));
            push_single(nc, b, m_rx(FRAC_PI_2));
            if theta.abs() > EPS {
                nc.push(NativeGate::Rzz(a, b, theta));
            }
            push_single(nc, a, m_rx(-FRAC_PI_2));
            push_single(nc, b, m_rx(-FRAC_PI_2));
        }
    }
}

/// CP(a, b, lambda) [diag(1,1,1,e^{i*lambda})] up to global phase ==
/// Rz(a, lambda/2) . Rz(b, lambda/2) . Rzz(a, b, -lambda/2).
/// (CZ is the lambda == pi special case.)
fn decompose_cp(nc: &mut NativeCircuit, a: usize, b: usize, lambda: f64) {
    let phi = lambda / 2.0;
    if phi.abs() > EPS {
        nc.push(NativeGate::Rz(a, phi));
        nc.push(NativeGate::Rz(b, phi));
    }
    if phi.abs() > EPS {
        nc.push(NativeGate::Rzz(a, b, -phi));
    }
}
/// Returns true if `a` and `b` are the same single-qubit unitary up to
/// an unobservable global phase. Used by this module's own ZYZ tests,
/// and reused crate-wide (see `ibm_export.rs`) by anything that proves
/// a gate-decomposition identity by multiplying matrices back together
/// and checking the product against a known-correct target -- the same
/// kind of check this module's decompositions are already verified
/// against in `tests/decompositions.rs`, just made reusable instead of
/// re-implemented per module.
pub(crate) fn approx_eq_up_to_global_phase(a: Mat2, b: Mat2) -> bool {
    let mut phase: Option<C> = None;
    'outer: for row in 0..2 {
        for col in 0..2 {
            if b[row][col].abs() > 1e-9 {
                let inv_mag_sq = 1.0 / (b[row][col].abs() * b[row][col].abs());
                let conj = C::new(b[row][col].re, -b[row][col].im);
                phase = Some(C::new(inv_mag_sq, 0.0) * (a[row][col] * conj));
                break 'outer;
            }
        }
    }
    let phase = match phase {
        Some(p) => p,
        // `b` is the zero matrix (never a real unitary in practice,
        // but mathematically "equal up to phase" to anything).
        None => return true,
    };
    if (phase.abs() - 1.0).abs() > 1e-6 {
        return false;
    }
    for row in 0..2 {
        for col in 0..2 {
            let rhs = phase * b[row][col];
            if (a[row][col].re - rhs.re).abs() > 1e-6 || (a[row][col].im - rhs.im).abs() > 1e-6 {
                return false;
            }
        }
    }
    true
}
