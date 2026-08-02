//! A subset OPENQASM 2.0 *and* 3.0 importer.
//!
//! This parses exactly the dialect `sirraya_qutub`'s own
//! `QuantumCircuit::to_qasm` / `QuantumRegister::to_qasm` and gate-builder
//! methods write (`h q[0];`, `rz(0.5) q[1];`, `rzz(1.2) q[0], q[2];`, ...),
//! plus the handful of standard `qelib1.inc`/`stdgates.inc` mnemonics
//! circuits exported from other tools (Qiskit, etc.) commonly use for
//! the same gate set. It is intentionally not a general OPENQASM
//! parser: no gate definitions, no classical control, no includes/
//! registers beyond a single qubit register and a single classical
//! register, no barriers. Anything outside that subset is a parse
//! error naming the offending line, rather than a silent skip.
//!
//! # QASM 2.0 vs. 3.0
//!
//! There is deliberately no version flag or separate entry point --
//! `parse` recognizes both dialects' spellings of the same handful of
//! constructs unconditionally, so a 2.0 program is parsed exactly the
//! way it always was (same code path, same behavior) and a 3.0
//! program's differently-spelled equivalents are additionally
//! recognized alongside it:
//!
//! - version header: `OPENQASM 2.0;` vs. `OPENQASM 3.0;` (or `3;`)
//! - include: `include "qelib1.inc";` vs. `include "stdgates.inc";`
//! - qubit register: `qreg q[5];` vs. `qubit[5] q;` (or bare `qubit q;`)
//! - classical register: `creg c[2];` vs. `bit[2] c;` (or bare `bit c;`)
//! - measure: `measure q[0] -> c[0];` vs. `c[0] = measure q[0];`
//!
//! Both the header and the include are already skipped unconditionally
//! (their contents were never inspected), so those two need no code
//! change. Gate-call syntax (`h q[0];`, `rz(0.5) q[1];`, ...) is
//! unchanged between the two dialects and already works either way.
//! Only the register declarations and the measure statement actually
//! differ in spelling, so those are the only two constructs `parse`
//! grows a second recognized spelling for. A source file can even
//! freely mix both spellings (e.g. a hand-edited file with a `qreg`
//! but an assignment-style `measure`) -- `parse` doesn't enforce
//! internal consistency of dialect, only that each individual
//! statement is one of the recognized forms.
//!
//! This crate's own QASM *emitters* (`crate::emit::to_qasm` /
//! `crate::emit::to_qasm3`, `crate::ibm_export::to_ibm_qasm`) each
//! commit to one dialect per function rather than mixing spellings --
//! this module's acceptance of either spelling is about being a
//! liberal *importer* of QASM written or exported by other tools, not
//! license for this crate's own writers to be inconsistent.

use crate::ir::{Circuit, Gate};

pub fn parse(source: &str) -> Result<Circuit, String> {
    let mut num_qubits: Option<usize> = None;
    let mut num_clbits: Option<usize> = None;
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
            let n = parse_register_size(rest, lineno)?;
            num_clbits = Some(n);
            circuit.num_clbits = n;
            continue;
        }
        // QASM 3.0 spellings of the same two declarations -- see this
        // module's doc comment. `qubit[5] q;` (or bare `qubit q;`,
        // implicit size 1) in place of `qreg q[5];`; `bit[2] c;` (or
        // bare `bit c;`) in place of `creg c[2];`.
        if let Some(rest) = stmt.strip_prefix("qubit") {
            let n = parse_qasm3_decl_size(rest, lineno)?;
            num_qubits = Some(n);
            circuit.num_qubits = n;
            continue;
        }
        if let Some(rest) = stmt.strip_prefix("bit") {
            let n = parse_qasm3_decl_size(rest, lineno)?;
            num_clbits = Some(n);
            circuit.num_clbits = n;
            continue;
        }
        if stmt.starts_with("//") {
            continue;
        }
        if let Some(rest) = stmt.strip_prefix("measure") {
            let n = num_qubits.ok_or_else(|| {
                format!("line {}: `measure` before a qubit register declaration: `{}`", lineno, stmt)
            })?;
            let c_count = num_clbits.ok_or_else(|| {
                format!("line {}: `measure` before a classical register declaration: `{}`", lineno, stmt)
            })?;
            let (q, c) = parse_measure_statement(rest, lineno)?;
            check_measure_range(q, n, c, c_count, lineno, stmt)?;
            circuit.gates.push(Gate::Measure(q, c));
            continue;
        }
        // QASM 3.0's assignment-style measure: `c[0] = measure q[0];`
        // in place of 2.0's `measure q[0] -> c[0];`. Only recognized
        // when the statement's right-hand side, after the `=`, starts
        // with `measure` -- nothing else in this subset's grammar uses
        // `=`, so this can't misfire on an ordinary gate call.
        if let Some(eq_pos) = stmt.find('=') {
            let rhs = stmt[eq_pos + 1..].trim_start();
            if let Some(rest) = rhs.strip_prefix("measure") {
                let n = num_qubits.ok_or_else(|| {
                    format!("line {}: `measure` before a qubit register declaration: `{}`", lineno, stmt)
                })?;
                let c_count = num_clbits.ok_or_else(|| {
                    format!("line {}: `measure` before a classical register declaration: `{}`", lineno, stmt)
                })?;
                let lhs = stmt[..eq_pos].trim();
                let c = parse_index_ref(lhs, lineno)?;
                let q = parse_index_ref(rest, lineno)?;
                check_measure_range(q, n, c, c_count, lineno, stmt)?;
                circuit.gates.push(Gate::Measure(q, c));
                continue;
            }
        }

        let n = num_qubits.ok_or_else(|| {
            format!(
                "line {}: gate before a qubit register declaration (`qreg`/`qubit`): `{}`",
                lineno, stmt
            )
        })?;
        let gate = parse_gate_statement(stmt, lineno)?;
        for q in gate.qubits() {
            if q >= n {
                return Err(format!(
                    "line {}: qubit index {} out of range for a {}-qubit register: `{}`",
                    lineno, q, n, stmt
                ));
            }
        }
        circuit.gates.push(gate);
    }

    if num_qubits.is_none() {
        return Err("no qubit register declaration (`qreg`/`qubit`) found".to_string());
    }
    Ok(circuit)
}

/// Range-checks a parsed `Measure`'s qubit/clbit indices against the
/// declared register sizes, shared by both the arrow-style (2.0) and
/// assignment-style (3.0) measure statements so the two dialects
/// report identically-worded errors.
fn check_measure_range(
    q: usize,
    num_qubits: usize,
    c: usize,
    num_clbits: usize,
    lineno: usize,
    stmt: &str,
) -> Result<(), String> {
    if q >= num_qubits {
        return Err(format!(
            "line {}: qubit index {} out of range for a {}-qubit register: `{}`",
            lineno, q, num_qubits, stmt
        ));
    }
    if c >= num_clbits {
        return Err(format!(
            "line {}: classical bit index {} out of range for a {}-bit register: `{}`",
            lineno, c, num_clbits, stmt
        ));
    }
    Ok(())
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

/// Parses a QASM 3.0-style register declaration's size, e.g. the
/// `[5] q` in `qubit[5] q;` or the `[2] c` in `bit[2] c;` (the size
/// comes *before* the name here, unlike `qreg`/`creg`'s `q[5]`, but
/// [`parse_register_size`] only ever looks for the first `[...]` pair
/// regardless of what surrounds it, so it already extracts the size
/// correctly from either shape and is reused as-is). Also accepts the
/// bracket-less single-qubit/single-bit form (`qubit q;`, `bit c;`),
/// which QASM 3.0 defines as an implicit size of 1.
fn parse_qasm3_decl_size(rest: &str, lineno: usize) -> Result<usize, String> {
    if rest.trim_start().starts_with('[') {
        parse_register_size(rest, lineno)
    } else {
        Ok(1)
    }
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
    rest.split(',').map(|tok| parse_index_ref(tok, lineno)).collect()
}

/// Parses a single `name[N]` reference (e.g. `q[0]` or `c[2]`), returning
/// just `N`. Shared by `parse_qubit_list` (comma-separated `q[N]`'s) and
/// `parse_measure_statement` (one `q[N]` and one `c[N]` either side of
/// `->`) so both stay in sync on what counts as a well-formed reference.
fn parse_index_ref(tok: &str, lineno: usize) -> Result<usize, String> {
    let tok = tok.trim();
    let open = tok
        .find('[')
        .ok_or_else(|| format!("line {}: expected `name[N]`, got `{}`", lineno, tok))?;
    let close = tok
        .find(']')
        .ok_or_else(|| format!("line {}: expected `name[N]`, got `{}`", lineno, tok))?;
    tok[open + 1..close]
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("line {}: bad index in `{}`", lineno, tok))
}

/// Parses the statement text after the `measure` keyword has already
/// been stripped, e.g. ` q[0] -> c[1]`. Returns `(qubit_index,
/// clbit_index)`; range-checking against the declared `qreg`/`creg`
/// sizes happens in the caller, the same way qubit indices are already
/// range-checked for every other gate statement.
fn parse_measure_statement(rest: &str, lineno: usize) -> Result<(usize, usize), String> {
    let arrow = rest.find("->").ok_or_else(|| {
        format!("line {}: `measure` statement missing `->`: `measure{}`", lineno, rest)
    })?;
    let qubit_part = &rest[..arrow];
    let clbit_part = &rest[arrow + 2..];
    let q = parse_index_ref(qubit_part, lineno)?;
    let c = parse_index_ref(clbit_part, lineno)?;
    Ok((q, c))
}

#[cfg(test)]
mod measure_tests {
    use super::*;

    #[test]
    fn parses_measure_statement() {
        let src = "OPENQASM 2.0;\nqreg q[2];\ncreg c[2];\nh q[0];\nmeasure q[0] -> c[0];\n";
        let circuit = parse(src).unwrap();
        assert_eq!(circuit.num_clbits, 2);
        assert_eq!(circuit.gates, vec![Gate::H(0), Gate::Measure(0, 0)]);
    }

    #[test]
    fn rejects_qubit_index_out_of_range_for_measure() {
        let src = "OPENQASM 2.0;\nqreg q[2];\ncreg c[2];\nmeasure q[5] -> c[0];\n";
        assert!(parse(src).is_err());
    }

    #[test]
    fn rejects_clbit_index_out_of_range_for_measure() {
        let src = "OPENQASM 2.0;\nqreg q[2];\ncreg c[2];\nmeasure q[0] -> c[9];\n";
        assert!(parse(src).is_err());
    }

    #[test]
    fn rejects_measure_before_creg_declaration() {
        let src = "OPENQASM 2.0;\nqreg q[2];\nmeasure q[0] -> c[0];\n";
        assert!(parse(src).is_err());
    }

    #[test]
    fn rejects_malformed_measure_missing_arrow() {
        let src = "OPENQASM 2.0;\nqreg q[2];\ncreg c[2];\nmeasure q[0] c[0];\n";
        assert!(parse(src).is_err());
    }
}

#[cfg(test)]
mod qasm3_tests {
    use super::*;

    #[test]
    fn parses_qasm3_register_declarations_and_assignment_measure() {
        let src = "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[2] q;\nbit[2] c;\nh q[0];\ncx q[0], q[1];\nc[0] = measure q[0];\nc[1] = measure q[1];\n";
        let circuit = parse(src).unwrap();
        assert_eq!(circuit.num_qubits, 2);
        assert_eq!(circuit.num_clbits, 2);
        assert_eq!(
            circuit.gates,
            vec![Gate::H(0), Gate::Cx(0, 1), Gate::Measure(0, 0), Gate::Measure(1, 1)]
        );
    }

    #[test]
    fn parses_bracket_less_single_qubit_and_bit_declarations() {
        // QASM 3.0's implicit-size-1 form: `qubit q;` / `bit c;`.
        let src = "OPENQASM 3.0;\nqubit q;\nbit c;\nh q[0];\nc[0] = measure q[0];\n";
        let circuit = parse(src).unwrap();
        assert_eq!(circuit.num_qubits, 1);
        assert_eq!(circuit.num_clbits, 1);
        assert_eq!(circuit.gates, vec![Gate::H(0), Gate::Measure(0, 0)]);
    }

    #[test]
    fn qasm2_input_is_unaffected_by_qasm3_support() {
        // Same source and expected result as the pre-existing
        // measure_tests::parses_measure_statement -- a direct
        // regression guard that adding QASM3 recognition didn't
        // change how QASM2 input is parsed.
        let src = "OPENQASM 2.0;\nqreg q[2];\ncreg c[2];\nh q[0];\nmeasure q[0] -> c[0];\n";
        let circuit = parse(src).unwrap();
        assert_eq!(circuit.num_clbits, 2);
        assert_eq!(circuit.gates, vec![Gate::H(0), Gate::Measure(0, 0)]);
    }

    #[test]
    fn rejects_qubit_index_out_of_range_for_qasm3_measure() {
        let src = "OPENQASM 3.0;\nqubit[2] q;\nbit[2] c;\nc[0] = measure q[5];\n";
        assert!(parse(src).is_err());
    }

    #[test]
    fn rejects_clbit_index_out_of_range_for_qasm3_measure() {
        let src = "OPENQASM 3.0;\nqubit[2] q;\nbit[2] c;\nc[9] = measure q[0];\n";
        assert!(parse(src).is_err());
    }

    #[test]
    fn rejects_qasm3_measure_before_bit_declaration() {
        let src = "OPENQASM 3.0;\nqubit[2] q;\nc[0] = measure q[0];\n";
        assert!(parse(src).is_err());
    }

    #[test]
    fn a_source_file_can_mix_qasm2_and_qasm3_spellings() {
        // parse() doesn't enforce dialect consistency -- see this
        // module's doc comment -- so a qreg declaration paired with
        // an assignment-style measure should still parse.
        let src = "OPENQASM 3.0;\nqreg q[1];\nbit[1] c;\nh q[0];\nc[0] = measure q[0];\n";
        let circuit = parse(src).unwrap();
        assert_eq!(circuit.gates, vec![Gate::H(0), Gate::Measure(0, 0)]);
    }

    #[test]
    fn gate_calls_are_unchanged_between_dialects() {
        let src = "OPENQASM 3.0;\nqubit[3] q;\nbit[3] c;\nh q[0];\nrz(0.5) q[1];\nrzz(1.2) q[0], q[2];\n";
        let circuit = parse(src).unwrap();
        assert_eq!(
            circuit.gates,
            vec![Gate::H(0), Gate::Rz(1, 0.5), Gate::Rzz(0, 2, 1.2)]
        );
    }
}
