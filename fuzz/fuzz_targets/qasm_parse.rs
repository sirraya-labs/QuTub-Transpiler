#![no_main]

//! Frontend robustness target. `qasm::parse` is the very first thing a
//! new contributor's circuit, or a malicious/malformed file, touches —
//! per the roadmap's own guidance ("don't only fuzz the parser", the
//! corollary is "but definitely also fuzz the parser"). This target
//! asserts one thing only: parse() must never panic, regardless of
//! input. Producing `Err(String)` on garbage input is the correct,
//! expected behavior and is not a finding.

use libfuzzer_sys::fuzz_target;
use sirraya_qutub_transpiler::qasm;

fuzz_target!(|data: &[u8]| {
    // Real QASM is UTF-8 text; lossily-decoded arbitrary bytes still
    // exercise the tokenizer/parser's handling of unexpected characters
    // without the fuzzer wasting most of its budget on inputs that are
    // rejected before reaching interesting code paths.
    let source = String::from_utf8_lossy(data);
    let _ = qasm::parse(&source);
});
