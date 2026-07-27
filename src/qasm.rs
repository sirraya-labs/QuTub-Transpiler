//! A subset OPENQASM 2.0 importer.
//!
//! This parses exactly the dialect `sirraya_qutub`'s own
//! `QuantumCircuit::to_qasm` / `QuantumRegister::to_qasm` and gate-builder
//! methods write (`h q[0];`, `rz(0.5) q[1];`, `rzz(1.2) q[0], q[2];`, ...),
//! plus the handful of standard `qelib1.inc` mnemonics circuits exported
//! from other tools (Qiskit, etc.) commonly use for the same gate set.
//! It is intentionally not a general OPENQASM parser: no gate
//! definitions, no classical control, no includes/registers beyond a
//! single `qreg`/`creg` pair, no barriers. Anything outside that subset
//! is a parse error naming the offending line, rather than a silent
//! skip.

use crate::ir::{Circuit, Gate};

pub fn parse(source: &str) -> Result<Circuit, String> {
    let mut num_qubits: Option<usize> = None;
    let mut circuit = Circuit::default();

    for (lineno, raw_stmt) in split_statements(source) {
        let stmt = raw_stmt.trim();
        if stmt.is_empty() {
            continue;
        }

        if stmt.starts_with("OPENQASM") {
            continue;
        }
        if stmt.starts_with("include") {
            continue;
        }
        if let Some(rest) = stmt.strip_prefix("qreg") {
            let n = parse_register_size(rest, lineno)?;
            num_qubits = Some(n);
            circuit.num_qubits = n;
            continue;
        }
        if let Some(rest) = stmt.strip_prefix("creg") {
            // Classical register: parsed for validity, not otherwise used.
            let _ = parse_register_size(rest, lineno)?;
            continue;
        }
        if stmt.starts_with("//") {
            continue;
        }

        let n = num_qubits
            .ok_or_else(|| format!("line {}: gate before `qreg` declaration: `{}`", lineno, stmt))?;
        let gate = parse_gate_statement(stmt, lineno)?;
        for q in gate.qubits() {
            if q >= n {
                return Err(format!(
                    "line {}: qubit index {} out of range for qreg[{}]: `{}`",
                    lineno, q, n, stmt
                ));
            }
        }
        circuit.gates.push(gate);
    }

    if num_qubits.is_none() {
        return Err("no `qreg` declaration found".to_string());
    }
    Ok(circuit)
}

/// Splits `; `-terminated statements out of the source, stripping `//`
/// line comments first, and returns each with its (1-indexed) source
/// line number for error messages.
fn split_statements(source: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_line = 1;

    for (line_idx, raw_line) in source.lines().enumerate() {
        let line_no = line_idx + 1;
        let code = match raw_line.find("//") {
            Some(pos) => &raw_line[..pos],
            None => raw_line,
        };
        if current.is_empty() {
            current_line = line_no;
        }
        current.push_str(code);
        current.push(' ');

        while let Some(pos) = current.find(';') {
            let stmt: String = current[..pos].to_string();
            out.push((current_line, stmt));
            current = current[pos + 1..].to_string();
            current_line = line_no;
        }
    }
    if !current.trim().is_empty() {
        out.push((current_line, current));
    }
    out
}

fn parse_register_size(rest: &str, lineno: usize) -> Result<usize, String> {
    let open = rest
        .find('[')
        .ok_or_else(|| format!("line {}: malformed register declaration", lineno))?;
    let close = rest
        .find(']')
        .ok_or_else(|| format!("line {}: malformed register declaration", lineno))?;
    rest[open + 1..close]
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("line {}: malformed register size", lineno))
}

/// Parses one gate statement, e.g. `cx q[0], q[1]` or `rz(0.3) q[2]`
/// (statement text has already had its trailing `;` stripped).
fn parse_gate_statement(stmt: &str, lineno: usize) -> Result<Gate, String> {
    let (name, after_name) = split_name(stmt);
    let (params, after_params) = split_params(after_name, lineno)?;
    let qubits = parse_qubit_list(after_params, lineno)?;

    let need = |k: usize| -> Result<(), String> {
        if qubits.len() != k {
            Err(format!(
                "line {}: `{}` expects {} qubit(s), got {}",
                lineno,
                name,
                k,
                qubits.len()
            ))
        } else {
            Ok(())
        }
    };
    let need_param = |k: usize| -> Result<f64, String> {
        params.get(k).copied().ok_or_else(|| {
            format!("line {}: `{}` is missing a numeric parameter", lineno, name)
        })
    };

    match name.as_str() {
        "h" => {
            need(1)?;
            Ok(Gate::H(qubits[0]))
        }
        "x" => {
            need(1)?;
            Ok(Gate::X(qubits[0]))
        }
        "y" => {
            need(1)?;
            Ok(Gate::Y(qubits[0]))
        }
        "z" => {
            need(1)?;
            Ok(Gate::Z(qubits[0]))
        }
        "s" => {
            need(1)?;
            Ok(Gate::S(qubits[0]))
        }
        "sdg" => {
            need(1)?;
            Ok(Gate::Sdg(qubits[0]))
        }
        "t" => {
            need(1)?;
            Ok(Gate::T(qubits[0]))
        }
        "tdg" => {
            need(1)?;
            Ok(Gate::Tdg(qubits[0]))
        }
        "rx" => {
            need(1)?;
            Ok(Gate::Rx(qubits[0], need_param(0)?))
        }
        "ry" => {
            need(1)?;
            Ok(Gate::Ry(qubits[0], need_param(0)?))
        }
        "rz" => {
            need(1)?;
            Ok(Gate::Rz(qubits[0], need_param(0)?))
        }
        "cx" | "cnot" => {
            need(2)?;
            Ok(Gate::Cx(qubits[0], qubits[1]))
        }
        "cz" => {
            need(2)?;
            Ok(Gate::Cz(qubits[0], qubits[1]))
        }
        "swap" => {
            need(2)?;
            Ok(Gate::Swap(qubits[0], qubits[1]))
        }
        "rxx" => {
            need(2)?;
            Ok(Gate::Rxx(qubits[0], qubits[1], need_param(0)?))
        }
        "ryy" => {
            need(2)?;
            Ok(Gate::Ryy(qubits[0], qubits[1], need_param(0)?))
        }
        "rzz" => {
            need(2)?;
            Ok(Gate::Rzz(qubits[0], qubits[1], need_param(0)?))
        }
        "cp" | "cphase" | "crz_phase" => {
            need(2)?;
            Ok(Gate::Cp(qubits[0], qubits[1], need_param(0)?))
        }
        other => Err(format!(
            "line {}: unsupported gate `{}` (statement: `{}`)",
            lineno, other, stmt
        )),
    }
}

fn split_name(stmt: &str) -> (String, &str) {
    let stmt = stmt.trim_start();
    let end = stmt
        .find(|c: char| c == '(' || c.is_whitespace())
        .unwrap_or(stmt.len());
    (stmt[..end].to_string(), &stmt[end..])
}

/// Parses an optional `(a, b, ...)` parameter list. Returns the
/// parsed f64 params and the remainder of the statement after the
/// closing paren (or the original remainder if there was none).
fn split_params(rest: &str, lineno: usize) -> Result<(Vec<f64>, &str), String> {
    let rest = rest.trim_start();
    if !rest.starts_with('(') {
        return Ok((Vec::new(), rest));
    }
    let close = rest
        .find(')')
        .ok_or_else(|| format!("line {}: unterminated parameter list", lineno))?;
    let inner = &rest[1..close];
    let params = inner
        .split(',')
        .map(|p| {
            p.trim()
                .parse::<f64>()
                .map_err(|_| format!("line {}: bad numeric parameter `{}`", lineno, p.trim()))
        })
        .collect::<Result<Vec<f64>, String>>()?;
    Ok((params, &rest[close + 1..]))
}

/// Parses a comma-separated list of `q[N]` references.
fn parse_qubit_list(rest: &str, lineno: usize) -> Result<Vec<usize>, String> {
    rest.split(',')
        .map(|tok| {
            let tok = tok.trim();
            let open = tok
                .find('[')
                .ok_or_else(|| format!("line {}: expected `q[N]`, got `{}`", lineno, tok))?;
            let close = tok
                .find(']')
                .ok_or_else(|| format!("line {}: expected `q[N]`, got `{}`", lineno, tok))?;
            tok[open + 1..close]
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("line {}: bad qubit index in `{}`", lineno, tok))
        })
        .collect()
}
