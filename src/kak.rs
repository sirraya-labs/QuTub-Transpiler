//! Cartan KAK decomposition of an arbitrary two-qubit unitary into the
//! crate's native `{Rz, Ry, Rzz}` gate set, using exactly 3 `Rzz`
//! entanglers -- optimal for a generic two-qubit unitary (matching the
//! textbook result that 3 CNOT-equivalent entanglers are necessary and
//! sufficient), versus whatever count [`crate::native`]'s fixed
//! per-gate-type identities happen to produce after
//! [`crate::optimize`]'s peephole cleanup. Intended to sit at the
//! "full-run resynthesis pass" seam [`crate::native`]'s module doc
//! already anticipates: once a source-level pass has merged a block of
//! gates acting on the same two physical qubits into a single dense 4x4
//! unitary, [`synthesize_two_qubit_unitary`] re-expresses that unitary
//! optimally, rather than decomposing gate-by-gate through the fixed
//! `Cx`/`Swap`/`Rxx`/`Ryy` identities.
//!
//! ## The math
//!
//! Any `U(4)` two-qubit unitary factors (Cartan's KAK decomposition for
//! `SU(4)`, e.g. Kraus & Cirac 2001, Zhang/Vala/Sastry/Whaley 2003) as
//!
//! ```text
//! U = (k1 kron k2) . exp(i(a XX + b YY + c ZZ)) . (k3 kron k4)
//! ```
//!
//! up to global phase, for single-qubit `k1,k2,k3,k4` and real
//! `a,b,c`. Since `X kron X`, `Y kron Y`, `Z kron Z` pairwise commute
//! (they share a joint eigenbasis -- the Bell/"magic" basis), the
//! middle factor splits into three independent pieces, each of which is
//! exactly a single `Rzz` conjugated by a fixed local change of axis --
//! the *same* exact identities [`crate::native`] already uses for `Rxx`
//! and `Ryy` (`X = H Z H`, `Y = Rx(pi/2) Z Rx(-pi/2)^dagger`), just
//! applied here to a `Ry`/`Rz`-synthesized coefficient instead of a
//! fixed angle:
//!
//! ```text
//! exp(i a XX) = (H kron H)   . Rzz(-2a) . (H kron H)
//! exp(i b YY) = (Rx(-pi/2) kron Rx(-pi/2)) . Rzz(-2b) . (Rx(pi/2) kron Rx(pi/2))
//! exp(i c ZZ) = Rzz(-2c)
//! ```
//!
//! (Signs follow `Rzz(theta) = exp(-i theta/2 ZZ)`, the same convention
//! [`crate::native::decompose_cp`]'s doc comment already establishes.)
//!
//! Extracting `(k1,k2,a,b,c,k3,k4)` from `U` uses the standard "magic
//! basis" trick: conjugating `U` by the change-of-basis matrix `M`
//! below turns local unitaries `k kron k'` into *real* `SO(4)` rotations
//! (this is the exceptional isomorphism `Spin(4) = SU(2) x SU(2)`), and
//! turns the canonical middle factor into a *diagonal* unitary (since
//! `M` simultaneously diagonalizes `XX`, `YY`, `ZZ` by construction).
//! Writing `U_B = M^dagger . U' . M` (`U'` = `U` normalized into
//! `SU(4)`) and `S = U_B^T U_B` (transpose, not dagger), `S` is
//! symmetric *and* unitary, which forces its real and imaginary parts
//! `X = Re(S)`, `Y = Im(S)` to be real, symmetric, and -- because
//! `S S^dagger = I` reduces to `X^2+Y^2=I` and `XY=YX` -- *commuting*.
//! Two commuting real symmetric matrices are exactly what a real
//! orthogonal change of basis can jointly diagonalize, which is what
//! recovers `k3 kron k4` (as the real rotation `O_R`) and the
//! `(a,b,c)` parameters (as the phases of `S`'s eigenvalues) without
//! ever needing a general complex eigensolver. `k1 kron k2` then falls
//! out algebraically as `U_B . O_R^T . D^dagger`, mapped back through
//! `M`.
//!
//! This closes the gap [`crate::native`]'s module doc flags as future
//! work ("if/when a full-run resynthesis pass is added, it can reuse
//! this same ZYZ algebra") for the two-qubit case specifically. It does
//! **not** implement Weyl-chamber canonicalization: the `(a,b,c)`
//! returned are *a* valid solution (branch-fixed so the four "magic
//! basis" eigenvalue phases are self-consistent), not necessarily the
//! lexicographically-reduced representative of the 12 equivalent
//! labelings a full Weyl-chamber reduction would produce. That only
//! matters for detecting the 0/1/2-entangler special-case loci (local
//! gates, CNOT-equivalent gates, etc. can in principle synthesize with
//! fewer than 3 `Rzz`s); this implementation always emits 3 `Rzz`s
//! (correctly reducing to fewer only when `a`, `b`, or `c` individually
//! land on ~0, which the existing [`crate::optimize`] peephole pass
//! already drops). Getting the true minimal-count special cases right
//! is flagged as follow-up work, same spirit as this crate's other
//! "closes a real gap without claiming more than it delivers" module
//! docs.

use crate::native::{m_rx, zyz_decompose, C, Mat2};
use crate::native::{NativeCircuit, NativeGate};

type Mat4 = [[C; 4]; 4];

const EPS: f64 = 1e-9;

/// `native.rs` doesn't currently expose an Hadamard builder beyond its
/// private `m_h()` used inside `decompose_gate`'s `Rxx`/`Cx` cases.
/// Duplicated here (rather than requiring a visibility change) since
/// it's a two-line constant matrix; everything else this module needs
/// from `native.rs` (`m_rx`, `zyz_decompose`) is already `pub(crate)`.
fn m_h() -> Mat2 {
    let f = 1.0 / std::f64::consts::SQRT_2;
    [
        [C::new(f, 0.0), C::new(f, 0.0)],
        [C::new(f, 0.0), C::new(-f, 0.0)],
    ]
}

fn mat4_zero() -> Mat4 {
    [[C::new(0.0, 0.0); 4]; 4]
}

fn matmul4(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = mat4_zero();
    for i in 0..4 {
        for j in 0..4 {
            let mut s = C::new(0.0, 0.0);
            for k in 0..4 {
                s = s + a[i][k] * b[k][j];
            }
            out[i][j] = s;
        }
    }
    out
}

fn dagger4(a: &Mat4) -> Mat4 {
    let mut out = mat4_zero();
    for i in 0..4 {
        for j in 0..4 {
            out[i][j] = a[j][i].conj();
        }
    }
    out
}

fn transpose4(a: &Mat4) -> Mat4 {
    let mut out = mat4_zero();
    for i in 0..4 {
        for j in 0..4 {
            out[i][j] = a[j][i];
        }
    }
    out
}

/// Kronecker product of two 2x2 matrices into the 4x4 two-qubit space,
/// `q0` the more-significant qubit (matches the same big-endian
/// convention `Gate::Cp`'s `diag(1,1,1,e^{i lambda})` already implies).
pub(crate) fn kron(a: &Mat2, b: &Mat2) -> Mat4 {
    let mut out = mat4_zero();
    for i in 0..2 {
        for j in 0..2 {
            for k in 0..2 {
                for l in 0..2 {
                    out[2 * i + k][2 * j + l] = a[i][j] * b[k][l];
                }
            }
        }
    }
    out
}

/// Gaussian-elimination determinant, adequate for the fixed 4x4 size
/// this module only ever needs it for.
fn det4(min: &Mat4) -> C {
    let mut m = *min;
    let mut det = C::new(1.0, 0.0);
    for col in 0..4 {
        let mut piv = col;
        let mut best = m[col][col].abs();
        for r in (col + 1)..4 {
            if m[r][col].abs() > best {
                best = m[r][col].abs();
                piv = r;
            }
        }
        if piv != col {
            m.swap(col, piv);
            det = C::new(0.0, 0.0) - det;
        }
        if m[col][col].abs() < 1e-14 {
            return C::new(0.0, 0.0);
        }
        det = det * m[col][col];
        let denom = m[col][col];
        let inv = denom.conj() * C::new(1.0 / (denom.abs() * denom.abs()), 0.0);
        for r in (col + 1)..4 {
            let factor = m[r][col] * inv;
            for cc in col..4 {
                m[r][cc] = m[r][cc] - factor * m[col][cc];
            }
        }
    }
    det
}

/// The magic (Bell-state) basis: conjugating by `M` turns any local
/// unitary `k kron k'` into a real `SO(4)` rotation, and simultaneously
/// diagonalizes `X kron X`, `Y kron Y`, `Z kron Z` (empirically checked
/// in this module's tests, not just asserted -- see
/// `magic_basis_diagonalizes_pauli_pairs`).
fn magic_basis() -> Mat4 {
    let f = 1.0 / std::f64::consts::SQRT_2;
    let z = C::new(0.0, 0.0);
    let r = |x: f64| C::new(x, 0.0);
    let ii = |x: f64| C::new(0.0, x);
    [
        [r(f), z, z, ii(f)],
        [z, ii(f), r(f), z],
        [z, ii(f), r(-f), z],
        [r(f), z, z, ii(-f)],
    ]
}

/// Real-symmetric 4x4 eigensolver (cyclic Jacobi rotations, run to
/// convergence). Returns `(V, eigenvalues)` with `V^T A V ==
/// diag(eigenvalues)`, `V`'s columns the orthonormal eigenvectors.
fn jacobi_eigen_4(a_in: &[[f64; 4]; 4]) -> ([[f64; 4]; 4], [f64; 4]) {
    let mut a = *a_in;
    let mut v = [[0.0f64; 4]; 4];
    for i in 0..4 {
        v[i][i] = 1.0;
    }
    for _sweep in 0..100 {
        let mut off = 0.0;
        for p in 0..4 {
            for q in (p + 1)..4 {
                off += a[p][q] * a[p][q];
            }
        }
        if off.sqrt() < 1e-14 {
            break;
        }
        for p in 0..4 {
            for q in (p + 1)..4 {
                if a[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = 0.5 * (2.0 * a[p][q]).atan2(a[q][q] - a[p][p]);
                let c = theta.cos();
                let s = theta.sin();
                for k in 0..4 {
                    let akp = a[k][p];
                    let akq = a[k][q];
                    a[k][p] = c * akp - s * akq;
                    a[k][q] = s * akp + c * akq;
                }
                for k in 0..4 {
                    let apk = a[p][k];
                    let aqk = a[q][k];
                    a[p][k] = c * apk - s * aqk;
                    a[q][k] = s * apk + c * aqk;
                }
                for k in 0..4 {
                    let vkp = v[k][p];
                    let vkq = v[k][q];
                    v[k][p] = c * vkp - s * vkq;
                    v[k][q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let eigs = [a[0][0], a[1][1], a[2][2], a[3][3]];
    (v, eigs)
}

/// `V^T A V` for a real matrix `A` and real orthogonal `V` -- used only
/// to double-check (not just assume) that a chosen `V` also
/// diagonalizes a *second* commuting matrix, not just the generic
/// combination it was computed from.
fn conjugate_real(a: &[[f64; 4]; 4], v: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    let mut vt_a = [[0.0f64; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += v[k][i] * a[k][j];
            }
            vt_a[i][j] = s;
        }
    }
    let mut out = [[0.0f64; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += vt_a[i][k] * v[k][j];
            }
            out[i][j] = s;
        }
    }
    out
}

/// Extracts `(A, B)` 2x2 such that `kron(A,B) == m` exactly. Only valid
/// when `m` genuinely *is* a pure Kronecker product (true here: `m` is
/// always `M . O . M^dagger` for `O` a real `SO(4)` rotation, which is
/// always exactly `k kron k'` for some single-qubit `k,k'`).
fn un_kron(m: &Mat4) -> (Mat2, Mat2) {
    let mut best_norm = -1.0;
    let (mut bi, mut bj) = (0, 0);
    for i in 0..2 {
        for j in 0..2 {
            let mut norm = 0.0;
            for k in 0..2 {
                for l in 0..2 {
                    let v = m[2 * i + k][2 * j + l];
                    norm += v.abs() * v.abs();
                }
            }
            if norm > best_norm {
                best_norm = norm;
                bi = i;
                bj = j;
            }
        }
    }
    let mut best_e = -1.0;
    let (mut pp, mut qq) = (0, 0);
    for k in 0..2 {
        for l in 0..2 {
            let v = m[2 * bi + k][2 * bj + l];
            if v.abs() > best_e {
                best_e = v.abs();
                pp = k;
                qq = l;
            }
        }
    }
    let scale = m[2 * bi + pp][2 * bj + qq];
    let inv_scale = scale.conj() * C::new(1.0 / (scale.abs() * scale.abs()), 0.0);
    let mut b = [[C::new(0.0, 0.0); 2]; 2];
    for k in 0..2 {
        for l in 0..2 {
            b[k][l] = m[2 * bi + k][2 * bj + l] * inv_scale;
        }
    }
    let mut a = [[C::new(0.0, 0.0); 2]; 2];
    for i in 0..2 {
        for j in 0..2 {
            a[i][j] = m[2 * i + pp][2 * j + qq];
        }
    }
    (a, b)
}

/// The result of decomposing a two-qubit unitary: `U == (k1 kron k2) .
/// exp(i(a XX + b YY + c ZZ)) . (k3 kron k4)` up to global phase.
pub(crate) struct Kak {
    pub k1: Mat2,
    pub k2: Mat2,
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub k3: Mat2,
    pub k4: Mat2,
}

/// Decomposes an arbitrary two-qubit unitary `u` per this module's doc
/// comment. Panics only if `u` fails to jointly diagonalize `Re(S)` and
/// `Im(S)` under every tried generic combination -- which would mean
/// `u` wasn't actually unitary, not a real degeneracy (see the
/// commuting-matrices argument in the module doc: any genuinely unitary
/// input's `X,Y` provably commute, so a generic linear combination's
/// eigenbasis provably diagonalizes both).
pub(crate) fn kak_decompose(u: &Mat4) -> Kak {
    let det = det4(u);
    let phase = det.arg() / 4.0;
    let s_inv = C::polar(1.0, -phase);
    let mut uprime = mat4_zero();
    for i in 0..4 {
        for j in 0..4 {
            uprime[i][j] = u[i][j] * s_inv;
        }
    }

    let m = magic_basis();
    let mdag = dagger4(&m);
    let ub = matmul4(&mdag, &matmul4(&uprime, &m));

    let ubt = transpose4(&ub);
    let s = matmul4(&ubt, &ub);

    let mut x = [[0.0f64; 4]; 4];
    let mut y = [[0.0f64; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            x[i][j] = s[i][j].re();
            y[i][j] = s[i][j].im();
        }
    }

    let mut v_mat = [[0.0f64; 4]; 4];
    let mut found = false;
    for t in [1.3126, 0.71933, 2.23517, 3.87211, 0.13579] {
        let mut z = [[0.0f64; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                z[i][j] = x[i][j] + t * y[i][j];
            }
        }
        let (v, _) = jacobi_eigen_4(&z);
        let xd = conjugate_real(&x, &v);
        let yd = conjugate_real(&y, &v);
        let mut off = 0.0;
        for i in 0..4 {
            for j in 0..4 {
                if i != j {
                    off += xd[i][j].abs() + yd[i][j].abs();
                }
            }
        }
        if off < 1e-7 {
            v_mat = v;
            found = true;
            break;
        }
    }
    assert!(
        found,
        "kak_decompose: input was not unitary (X,Y failed to commute under every generic combination tried)"
    );

    let xd = conjugate_real(&x, &v_mat);
    let yd = conjugate_real(&y, &v_mat);

    let mut theta = [0.0f64; 4];
    for j in 0..4 {
        theta[j] = 0.5 * yd[j][j].atan2(xd[j][j]);
    }
    let total: f64 = theta.iter().sum();
    theta[3] -= total; // branch-fix: forces the four phases self-consistent

    let a = (theta[0] + theta[1]) / 2.0;
    let b = (theta[1] + theta[3]) / 2.0;
    let c = (theta[0] + theta[3]) / 2.0;

    let d_core: Mat4 = {
        let mut d = mat4_zero();
        for (k, th) in theta.iter().enumerate() {
            d[k][k] = C::polar(1.0, *th);
        }
        d
    };

    let mut o_r = mat4_zero();
    for i in 0..4 {
        for j in 0..4 {
            o_r[i][j] = C::new(v_mat[j][i], 0.0);
        }
    }
    if det4(&o_r).re() < 0.0 {
        for j in 0..4 {
            o_r[0][j] = C::new(0.0, 0.0) - o_r[0][j];
        }
    }

    let o_r_t = transpose4(&o_r);
    let d_core_inv: Mat4 = {
        let mut d = mat4_zero();
        for k in 0..4 {
            d[k][k] = d_core[k][k].conj();
        }
        d
    };
    let o_l = matmul4(&ub, &matmul4(&o_r_t, &d_core_inv));

    let k12 = matmul4(&m, &matmul4(&o_l, &mdag));
    let k34 = matmul4(&m, &matmul4(&o_r, &mdag));
    let (k1, k2) = un_kron(&k12);
    let (k3, k4) = un_kron(&k34);

    Kak { k1, k2, a, b, c, k3, k4 }
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

/// Emits the KAK decomposition of `u` (a two-qubit unitary acting on
/// physical/logical qubits `q0, q1`) as native `{Rz, Ry, Rzz}` gates:
/// `k3,k4` first, then the 3-`Rzz` canonical core, then `k1,k2` --
/// exactly 3 entanglers, optimal for a generic two-qubit unitary.
pub(crate) fn synthesize_two_qubit_unitary(u: &Mat4, q0: usize, q1: usize) -> NativeCircuit {
    let kak = kak_decompose(u);
    let mut nc = NativeCircuit::new(q0.max(q1) + 1);

    push_single(&mut nc, q0, kak.k3);
    push_single(&mut nc, q1, kak.k4);

    // exp(i a XX) = (H kron H) . Rzz(-2a) . (H kron H)
    let h = m_h();
    push_single(&mut nc, q0, h);
    push_single(&mut nc, q1, h);
    if kak.a.abs() > EPS {
        nc.push(NativeGate::Rzz(q0, q1, -2.0 * kak.a));
    }
    push_single(&mut nc, q0, h);
    push_single(&mut nc, q1, h);

    // exp(i b YY) = (Rx(-pi/2) kron Rx(-pi/2)) . Rzz(-2b) . (Rx(pi/2) kron Rx(pi/2))
    let rxp = m_rx(std::f64::consts::FRAC_PI_2);
    let rxm = m_rx(-std::f64::consts::FRAC_PI_2);
    push_single(&mut nc, q0, rxp);
    push_single(&mut nc, q1, rxp);
    if kak.b.abs() > EPS {
        nc.push(NativeGate::Rzz(q0, q1, -2.0 * kak.b));
    }
    push_single(&mut nc, q0, rxm);
    push_single(&mut nc, q1, rxm);

    // exp(i c ZZ) = Rzz(-2c)
    if kak.c.abs() > EPS {
        nc.push(NativeGate::Rzz(q0, q1, -2.0 * kak.c));
    }

    push_single(&mut nc, q0, kak.k1);
    push_single(&mut nc, q1, kak.k2);

    nc
}

// ---------------------------------------------------------------------
// Two-qubit block resynthesis over an already-decomposed NativeCircuit
// -- the "full-run resynthesis pass" seam `native.rs`'s module doc
// anticipates and this module's own doc comment targets. `native.rs`'s
// per-gate identities (one fixed `{Rz,Ry,Rzz}` expansion per source
// `Cx`/`Cz`/`Swap`/`Rxx`/`Ryy`/`Cp`) are each individually exact, but
// when several of them land on the *same* qubit pair back to back their
// `Rzz` counts just add (e.g. a `Swap` followed by a `Cx` on the same
// wires costs 3 + 1 = 4 `Rzz`'s), even though the textbook KAK bound
// says any single two-qubit unitary -- including that whole combined
// run -- never needs more than 3. This pass finds every maximal run of
// `Rz`/`Ry`/`Rzz` confined to one qubit pair, and, if it contains 2 or
// more `Rzz`'s (i.e. it came from more than one contributing two-qubit
// gate, or from a chain `optimize.rs`'s peephole pass didn't fully
// fuse), replaces the whole run with `synthesize_two_qubit_unitary`'s
// output instead. A run with 0 or 1 `Rzz`'s is already exactly what a
// single source gate's own identity produces, so it's left untouched
// rather than paying matrix-construction cost for no possible gain.
//
// Every other qubit's gates, and anything inside a run that doesn't
// qualify, pass through completely unchanged, in their original
// relative order -- safe because, by construction (see
// `resynthesize_native_circuit` below), nothing that isn't a member of
// a given run ever touches that run's qubit pair while the run is
// open, so everything surrounding a run is on disjoint qubits and
// commutes freely around it (the same disjoint-commutativity argument
// `ir_optimize.rs`'s module doc already relies on).
// ---------------------------------------------------------------------

use crate::native::{m_identity, m_ry, m_rz};
use std::collections::{HashMap, HashSet};

fn identity4() -> Mat4 {
    kron(&m_identity(), &m_identity())
}

/// `Rzz(theta)`'s matrix, `exp(-i*theta/2*ZZ)` -- diagonal in the
/// computational basis regardless of which of the pair is "more
/// significant". Unlike `Cx`, `ZZ` is symmetric under qubit exchange
/// (see this module's doc comment on the canonical core), so there's
/// no order-dependent case to handle the way `native.rs::decompose_cp`
/// has to for control/target.
fn rzz_matrix(theta: f64) -> Mat4 {
    let mut m = mat4_zero();
    let half = theta / 2.0;
    m[0][0] = C::polar(1.0, -half);
    m[1][1] = C::polar(1.0, half);
    m[2][2] = C::polar(1.0, half);
    m[3][3] = C::polar(1.0, -half);
    m
}

/// Embeds a single-qubit matrix `m` (acting on qubit `q`) into the
/// two-qubit space for a block whose "more significant" slot is `lo`.
fn embed_single(q: usize, lo: usize, m: Mat2) -> Mat4 {
    if q == lo {
        kron(&m, &m_identity())
    } else {
        kron(&m_identity(), &m)
    }
}

/// An in-progress maximal run confined to qubit pair `(lo, hi)`,
/// `lo < hi`: the accumulated unitary of every member gate seen so far
/// (first-applied-first, same convention as `matmul`/`matmul4`
/// throughout this crate), how many of those members were `Rzz`
/// (only >= 2 triggers a rewrite), and which gate indices (into the
/// original circuit) belong to it.
struct BlockInProgress {
    lo: usize,
    hi: usize,
    matrix: Mat4,
    rzz_count: usize,
    gate_indices: Vec<usize>,
}

/// A run that closed with `rzz_count >= 2`, ready to be replaced by its
/// KAK synthesis on the emission pass.
struct FinishedBlock {
    lo: usize,
    hi: usize,
    matrix: Mat4,
}

/// Closes the open block at `blocks[bid]`: frees its qubits (so a
/// later run on either one starts fresh) and, only if it qualifies
/// (`rzz_count >= 2`), tags every member gate index in `gate_block`
/// with the new finished-block id so the emission pass replaces them.
/// A no-op if `bid` was already closed (defends against `bid` being
/// visited twice via `owner`'s two qubit entries).
fn close_block(
    bid: usize,
    blocks: &mut [Option<BlockInProgress>],
    owner: &mut HashMap<usize, usize>,
    finished: &mut Vec<FinishedBlock>,
    gate_block: &mut [Option<usize>],
) {
    let blk = match blocks[bid].take() {
        Some(b) => b,
        None => return,
    };
    owner.remove(&blk.lo);
    owner.remove(&blk.hi);
    if blk.rzz_count >= 2 {
        let fbid = finished.len();
        for idx in &blk.gate_indices {
            gate_block[*idx] = Some(fbid);
        }
        finished.push(FinishedBlock {
            lo: blk.lo,
            hi: blk.hi,
            matrix: blk.matrix,
        });
    }
}

/// Re-synthesizes every maximal same-pair `{Rz, Ry, Rzz}` run in
/// `circuit` that contains 2+ `Rzz`'s into the KAK-optimal 3-`Rzz` form
/// (see this module's doc comment); everything else passes through
/// unchanged. Meant to run immediately after [`crate::native::decompose`],
/// before any backend-specific re-expansion (see `crate::backend::lower`).
pub fn resynthesize_native_circuit(circuit: &NativeCircuit) -> NativeCircuit {
    let gates = &circuit.gates;
    let n = gates.len();

    let mut blocks: Vec<Option<BlockInProgress>> = Vec::new();
    let mut owner: HashMap<usize, usize> = HashMap::new();
    let mut finished: Vec<FinishedBlock> = Vec::new();
    let mut gate_block: Vec<Option<usize>> = vec![None; n];

    for (i, g) in gates.iter().enumerate() {
        match *g {
            NativeGate::Rz(q, theta) => {
                if let Some(&bid) = owner.get(&q) {
                    let blk = blocks[bid].as_mut().unwrap();
                    let m4 = embed_single(q, blk.lo, m_rz(theta));
                    blk.matrix = matmul4(&m4, &blk.matrix);
                    blk.gate_indices.push(i);
                }
            }
            NativeGate::Ry(q, theta) => {
                if let Some(&bid) = owner.get(&q) {
                    let blk = blocks[bid].as_mut().unwrap();
                    let m4 = embed_single(q, blk.lo, m_ry(theta));
                    blk.matrix = matmul4(&m4, &blk.matrix);
                    blk.gate_indices.push(i);
                }
            }
            NativeGate::Rzz(a, b, theta) => {
                let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                let matching = match (owner.get(&lo), owner.get(&hi)) {
                    (Some(&x), Some(&y)) if x == y => {
                        let blk = blocks[x].as_ref().unwrap();
                        (blk.lo == lo && blk.hi == hi).then_some(x)
                    }
                    _ => None,
                };
                let bid = match matching {
                    Some(x) => x,
                    None => {
                        if let Some(&x) = owner.get(&lo) {
                            close_block(x, &mut blocks, &mut owner, &mut finished, &mut gate_block);
                        }
                        if let Some(&y) = owner.get(&hi) {
                            close_block(y, &mut blocks, &mut owner, &mut finished, &mut gate_block);
                        }
                        let new_id = blocks.len();
                        blocks.push(Some(BlockInProgress {
                            lo,
                            hi,
                            matrix: identity4(),
                            rzz_count: 0,
                            gate_indices: Vec::new(),
                        }));
                        owner.insert(lo, new_id);
                        owner.insert(hi, new_id);
                        new_id
                    }
                };
                let blk = blocks[bid].as_mut().unwrap();
                blk.matrix = matmul4(&rzz_matrix(theta), &blk.matrix);
                blk.rzz_count += 1;
                blk.gate_indices.push(i);
            }
        }
    }

    let remaining: HashSet<usize> = owner.values().copied().collect();
    for bid in remaining {
        close_block(bid, &mut blocks, &mut owner, &mut finished, &mut gate_block);
    }

    let mut nc = NativeCircuit::new(circuit.num_qubits);
    let mut emitted: HashSet<usize> = HashSet::new();
    for (i, g) in gates.iter().enumerate() {
        match gate_block[i] {
            Some(fbid) => {
                if emitted.insert(fbid) {
                    let fb = &finished[fbid];
                    let synthesized = synthesize_two_qubit_unitary(&fb.matrix, fb.lo, fb.hi);
                    nc.extend(synthesized.gates);
                }
            }
            None => nc.push(*g),
        }
    }
    nc
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    fn unitary_fidelity4(a: &Mat4, b: &Mat4) -> f64 {
        let ad = dagger4(a);
        let prod = matmul4(&ad, b);
        let mut tr = C::new(0.0, 0.0);
        for i in 0..4 {
            tr = tr + prod[i][i];
        }
        let f = tr.abs() / 4.0;
        f * f
    }

    fn mat2_mul(p: &Mat2, q: &Mat2) -> Mat2 {
        let mut out = [[C::new(0.0, 0.0); 2]; 2];
        for i in 0..2 {
            for j in 0..2 {
                let mut s = C::new(0.0, 0.0);
                for k in 0..2 {
                    s = s + p[i][k] * q[k][j];
                }
                out[i][j] = s;
            }
        }
        out
    }

    fn m_rz_local(theta: f64) -> Mat2 {
        [
            [C::polar(1.0, -theta / 2.0), C::new(0.0, 0.0)],
            [C::new(0.0, 0.0), C::polar(1.0, theta / 2.0)],
        ]
    }
    fn m_ry_local(theta: f64) -> Mat2 {
        let (c, s) = ((theta / 2.0).cos(), (theta / 2.0).sin());
        [
            [C::new(c, 0.0), C::new(-s, 0.0)],
            [C::new(s, 0.0), C::new(c, 0.0)],
        ]
    }

    /// A generic random single-qubit unitary via an arbitrary ZYZ triple
    /// (independent of `zyz_decompose`, so this doesn't test against
    /// itself).
    fn random_su2(rng: &mut impl Rng) -> Mat2 {
        let a: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
        let b: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
        let c: f64 = rng.gen_range(0.0..std::f64::consts::TAU);
        mat2_mul(&m_rz_local(a), &mat2_mul(&m_ry_local(b), &m_rz_local(c)))
    }

    fn reconstruct(kak: &Kak) -> Mat4 {
        let x: Mat2 = [[C::new(0.0,0.0), C::new(1.0,0.0)], [C::new(1.0,0.0), C::new(0.0,0.0)]];
        let y: Mat2 = [[C::new(0.0,0.0), C::new(0.0,-1.0)], [C::new(0.0,1.0), C::new(0.0,0.0)]];
        let z: Mat2 = [[C::new(1.0,0.0), C::new(0.0,0.0)], [C::new(0.0,0.0), C::new(-1.0,0.0)]];
        let xx = kron(&x,&x); let yy = kron(&y,&y); let zz = kron(&z,&z);
        let exp_i_herm = |gen: &Mat4, coeff: f64| -> Mat4 {
            let mut out = mat4_zero();
            for i in 0..4 { for j in 0..4 {
                let ident = if i==j { C::new(1.0,0.0) } else { C::new(0.0,0.0) };
                out[i][j] = ident * C::new(coeff.cos(),0.0) + gen[i][j] * C::new(0.0, coeff.sin());
            }}
            out
        };
        let core = matmul4(&exp_i_herm(&zz, kak.c), &matmul4(&exp_i_herm(&yy, kak.b), &exp_i_herm(&xx, kak.a)));
        matmul4(&kron(&kak.k1,&kak.k2), &matmul4(&core, &kron(&kak.k3,&kak.k4)))
    }

    #[test]
    fn magic_basis_diagonalizes_pauli_pairs() {
        let x: Mat2 = [[C::new(0.0,0.0), C::new(1.0,0.0)], [C::new(1.0,0.0), C::new(0.0,0.0)]];
        let y: Mat2 = [[C::new(0.0,0.0), C::new(0.0,-1.0)], [C::new(0.0,1.0), C::new(0.0,0.0)]];
        let z: Mat2 = [[C::new(1.0,0.0), C::new(0.0,0.0)], [C::new(0.0,0.0), C::new(-1.0,0.0)]];
        let m = magic_basis();
        let mdag = dagger4(&m);
        for (gen, expect) in [
            (kron(&x,&x), [1.0,1.0,-1.0,-1.0]),
            (kron(&y,&y), [-1.0,1.0,-1.0,1.0]),
            (kron(&z,&z), [1.0,-1.0,-1.0,1.0]),
        ] {
            let conj = matmul4(&mdag, &matmul4(&gen, &m));
            for i in 0..4 { for j in 0..4 {
                if i == j {
                    assert!((conj[i][j].re() - expect[i]).abs() < 1e-9);
                    assert!(conj[i][j].im().abs() < 1e-9);
                } else {
                    assert!(conj[i][j].abs() < 1e-9);
                }
            }}
        }
    }

    #[test]
    fn reconstructs_random_two_qubit_unitaries_exactly() {
        let mut rng = rand::thread_rng();
        for _ in 0..2000 {
            let k1 = random_su2(&mut rng);
            let k2 = random_su2(&mut rng);
            let k3 = random_su2(&mut rng);
            let k4 = random_su2(&mut rng);
            let a: f64 = rng.gen_range(-1.5..1.5);
            let b: f64 = rng.gen_range(-1.5..1.5);
            let c: f64 = rng.gen_range(-1.5..1.5);
            let u = reconstruct(&Kak { k1, k2, a, b, c, k3, k4 });

            let kak = kak_decompose(&u);
            let rec = reconstruct(&kak);
            let fid = unitary_fidelity4(&u, &rec);
            assert!((fid - 1.0).abs() < 1e-6, "fidelity {} at a={} b={} c={}", fid, a, b, c);
        }
    }

    #[test]
    fn known_gates_land_on_their_textbook_canonical_class() {
        // CX: canonical class (pi/4, 0, 0) up to local gates.
        let mut cx = mat4_zero();
        cx[0][0] = C::new(1.0,0.0); cx[1][1] = C::new(1.0,0.0);
        cx[2][3] = C::new(1.0,0.0); cx[3][2] = C::new(1.0,0.0);
        let kak = kak_decompose(&cx);
        assert!(kak.b.abs() < 1e-6 && kak.c.abs() < 1e-6);
        assert!((kak.a.abs() - std::f64::consts::FRAC_PI_4).abs() < 1e-6);

        // SWAP: canonical class (pi/4, pi/4, pi/4).
        let mut swap = mat4_zero();
        swap[0][0] = C::new(1.0,0.0); swap[1][2] = C::new(1.0,0.0);
        swap[2][1] = C::new(1.0,0.0); swap[3][3] = C::new(1.0,0.0);
        let kak = kak_decompose(&swap);
        assert!((kak.a.abs()-std::f64::consts::FRAC_PI_4).abs()<1e-6);
        assert!((kak.b.abs()-std::f64::consts::FRAC_PI_4).abs()<1e-6);
        assert!((kak.c.abs()-std::f64::consts::FRAC_PI_4).abs()<1e-6);
    }

    /// Simulates a `NativeCircuit` known to act only on qubits `q0,q1`
    /// as a dense 4x4 matrix, for testing `synthesize_two_qubit_unitary`'s
    /// output independently of the real `sirraya_qutub` dependency.
    fn simulate_native_circuit_as_mat4(circuit: &NativeCircuit, q0: usize, q1: usize) -> Mat4 {
        let ident2: Mat2 = [
            [C::new(1.0, 0.0), C::new(0.0, 0.0)],
            [C::new(0.0, 0.0), C::new(1.0, 0.0)],
        ];
        let embed_single = |q: usize, m: Mat2| -> Mat4 {
            if q == q0 {
                kron(&m, &ident2)
            } else {
                kron(&ident2, &m)
            }
        };
        let mut acc = mat4_zero();
        for i in 0..4 {
            acc[i][i] = C::new(1.0, 0.0);
        }
        for gate in &circuit.gates {
            let g: Mat4 = match *gate {
                NativeGate::Rz(q, theta) => embed_single(q, m_rz_local(theta)),
                NativeGate::Ry(q, theta) => embed_single(q, m_ry_local(theta)),
                NativeGate::Rzz(_, _, theta) => {
                    let z: Mat2 = [
                        [C::new(1.0, 0.0), C::new(0.0, 0.0)],
                        [C::new(0.0, 0.0), C::new(-1.0, 0.0)],
                    ];
                    let zz = kron(&z, &z);
                    let mut out = mat4_zero();
                    for i in 0..4 {
                        for j in 0..4 {
                            let ident = if i == j { C::new(1.0, 0.0) } else { C::new(0.0, 0.0) };
                            out[i][j] = ident * C::new((theta / 2.0).cos(), 0.0)
                                + zz[i][j] * C::new(0.0, -(theta / 2.0).sin());
                        }
                    }
                    out
                }
            };
            acc = matmul4(&g, &acc); // gate list order: first-applied-first
        }
        acc
    }

    #[test]
    fn resynthesis_preserves_action_on_a_multi_rzz_block() {
        // A run with 3 Rzz's confined to the same pair, plus some
        // single-qubit gates interleaved -- exactly the case a single
        // source-level gate never produces on its own, but a run of
        // several adjacent two-qubit source gates would.
        let mut nc = NativeCircuit::new(2);
        nc.push(NativeGate::Rz(0, 0.3));
        nc.push(NativeGate::Rzz(0, 1, 0.5));
        nc.push(NativeGate::Ry(1, 0.2));
        nc.push(NativeGate::Rzz(1, 0, -0.7)); // reversed qubit order on purpose
        nc.push(NativeGate::Rz(1, -0.1));
        nc.push(NativeGate::Rzz(0, 1, 0.9));
        nc.push(NativeGate::Ry(0, 0.4));

        let resynth = resynthesize_native_circuit(&nc);

        let before = simulate_native_circuit_as_mat4(&nc, 0, 1);
        let after = simulate_native_circuit_as_mat4(&resynth, 0, 1);
        let fid = unitary_fidelity4(&before, &after);
        assert!((fid - 1.0).abs() < 1e-9, "fidelity {} after resynthesis", fid);

        let (_, two_count) = resynth.gate_counts();
        assert!(
            two_count <= 3,
            "expected at most 3 Rzz's after KAK resynthesis, got {}",
            two_count
        );
    }

    #[test]
    fn single_rzz_block_is_left_untouched() {
        // Only 1 Rzz in the run -- already exactly what a single source
        // gate's own identity produces, so resynthesis should be a
        // pure no-op (not just fidelity-preserving, byte-for-byte
        // identical).
        let mut nc = NativeCircuit::new(2);
        nc.push(NativeGate::Rz(0, 0.3));
        nc.push(NativeGate::Rzz(0, 1, 0.5));
        nc.push(NativeGate::Ry(1, 0.2));

        let resynth = resynthesize_native_circuit(&nc);
        assert_eq!(resynth.gates, nc.gates);
    }

    #[test]
    fn disjoint_qubit_gates_pass_through_and_dont_join_a_block() {
        // Two separate multi-Rzz blocks, on (0,1) and on (2,3), plus a
        // lone single-qubit gate on qubit 4 interleaved -- nothing here
        // should bleed into anything else's block.
        let mut nc = NativeCircuit::new(5);
        nc.push(NativeGate::Rzz(0, 1, 0.1));
        nc.push(NativeGate::Rz(4, 1.23)); // fully unrelated qubit
        nc.push(NativeGate::Rzz(2, 3, 0.2));
        nc.push(NativeGate::Rzz(0, 1, 0.3));
        nc.push(NativeGate::Rzz(2, 3, 0.4));

        let resynth = resynthesize_native_circuit(&nc);

        // The unrelated Rz(4, ...) must still appear, untouched.
        assert!(resynth
            .gates
            .iter()
            .any(|g| matches!(*g, NativeGate::Rz(4, a) if (a - 1.23).abs() < 1e-12)));

        // Each pair's Rzz count individually respects the KAK bound.
        let count_pair = |a: usize, b: usize| {
            resynth
                .gates
                .iter()
                .filter(|g| matches!(**g, NativeGate::Rzz(x, y, _) if (x, y) == (a, b) || (x, y) == (b, a)))
                .count()
        };
        assert!(count_pair(0, 1) <= 3);
        assert!(count_pair(2, 3) <= 3);
    }

    #[test]
    fn emitted_gate_circuit_matches_the_decomposed_unitary() {
        // Runs the *actual* emitted NativeCircuit (H/Rx-conjugated Rzz's,
        // not just the abstract exp(i(aXX+bYY+cZZ)) formula) through the
        // crate's own emit-free matrix semantics and checks it matches.
        let mut rng = rand::thread_rng();
        for _ in 0..500 {
            let k1 = random_su2(&mut rng);
            let k2 = random_su2(&mut rng);
            let k3 = random_su2(&mut rng);
            let k4 = random_su2(&mut rng);
            let a: f64 = rng.gen_range(-1.5..1.5);
            let b: f64 = rng.gen_range(-1.5..1.5);
            let c: f64 = rng.gen_range(-1.5..1.5);
            let u = reconstruct(&Kak { k1, k2, a, b, c, k3, k4 });

            let circuit = synthesize_two_qubit_unitary(&u, 0, 1);
            let simulated = simulate_native_circuit_as_mat4(&circuit, 0, 1);
            let fid = unitary_fidelity4(&u, &simulated);
            assert!((fid - 1.0).abs() < 1e-6, "circuit fidelity {} at a={} b={} c={}", fid, a, b, c);
        }
    }
}
