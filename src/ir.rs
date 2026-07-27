//! Source intermediate representation: the gate set a QASM 2.0 program
//! (or any other frontend) is parsed into, before native-gate
//! decomposition. This is deliberately a *rich* gate set (mirrors
//! `sirraya_qutub::core::QuantumRegister`'s `apply_*` surface) -- the
//! narrowing to a hardware-native set happens in [`crate::native`].

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
}

impl Gate {
    /// Qubit indices this gate touches, in the order they appear in the
    /// gate's own argument list (control(s) before target, as written).
    pub fn qubits(&self) -> Vec<usize> {
        use Gate::*;
        match *self {
            H(q) | X(q) | Y(q) | Z(q) | S(q) | Sdg(q) | T(q) | Tdg(q) | Rx(q, _) | Ry(q, _)
            | Rz(q, _) => vec![q],
            Cx(a, b) | Cz(a, b) | Swap(a, b) | Rxx(a, b, _) | Ryy(a, b, _) | Rzz(a, b, _)
            | Cp(a, b, _) => vec![a, b],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Circuit {
    pub num_qubits: usize,
    pub gates: Vec<Gate>,
}

impl Circuit {
    pub fn new(num_qubits: usize) -> Self {
        Self {
            num_qubits,
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
            };
            *counts.entry(name).or_insert(0) += 1;
        }
        counts
    }
}
