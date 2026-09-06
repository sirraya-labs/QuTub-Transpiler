//! Regression tests for fuzzer-discovered crashes in the QASM parser.
//! These ensure the same malformed inputs never cause panics again.

use sirraya_qutub_transpiler::qasm;

#[test]
fn rejects_closing_bracket_before_opening() {
    // This input caused a panic because ']' appeared before '['
    let invalid = "qreg q] [2];";
    let result = qasm::parse(invalid);
    assert!(result.is_err());
}

#[test]
fn rejects_empty_index() {
    let invalid = "qreg q[];";
    let result = qasm::parse(invalid);
    assert!(result.is_err());
}

#[test]
fn rejects_index_with_only_whitespace() {
    let invalid = "qreg q[ ];";
    let result = qasm::parse(invalid);
    assert!(result.is_err());
}
