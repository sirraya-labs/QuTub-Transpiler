//! Golden-file (snapshot) tests over the IR of a handful of canonical
//! circuits, at each pipeline stage. This is a different guardrail from
//! `verify_equivalence.rs`: fidelity checking proves a rewrite is
//! *semantically* correct but says nothing about *what changed* — a
//! pass could start producing a different (still-equivalent) gate
//! sequence, decomposition, or SWAP pattern and the equivalence oracle
//! would happily pass while a reviewer has no visibility into the
//! behavior change at all.
//!
//! `insta` snapshots the `Debug` output of the IR after each stage.
//! When a pass intentionally changes behavior, the diff shows up in the
//! PR itself (`cargo insta review` regenerates + the reviewer approves
//! the new snapshot as part of the PR, same as any other file change).
//! When a pass *unintentionally* changes behavior, the same diff is the
//! bug report.
//!
//! Uses `insta = { version = "1", features = ["yaml"] }` as a
//! dev-dependency. Run `cargo insta test` locally; `cargo insta review`
//! to accept intentional changes (writes straight into `tests/snapshots/`).

use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::{decompose, lower, optimize, optimize_ir, Backend};

fn bell() -> Circuit {
    let mut c = Circuit::new(2);
    c.gates.push(Gate::H(0));
    c.gates.push(Gate::Cx(0, 1));
    c.num_clbits = 2;
    c.gates.push(Gate::Measure(0, 0));
    c.gates.push(Gate::Measure(1, 1));
    c
}

fn ghz(n: usize) -> Circuit {
    let mut c = Circuit::new(n);
    c.gates.push(Gate::H(0));
    for i in 1..n {
        c.gates.push(Gate::Cx(0, i));
    }
    c
}

fn qft(n: usize) -> Circuit {
    // Textbook QFT: for each qubit, H then controlled phase rotations
    // from every later qubit — deliberately not the optimized form, so
    // `optimize_ir` has real cancellation/commuting work to do on it.
    let mut c = Circuit::new(n);
    for i in 0..n {
        c.gates.push(Gate::H(i));
        for j in (i + 1)..n {
            let angle = std::f64::consts::PI / (1u32 << (j - i)) as f64;
            c.gates.push(Gate::Cp(j, i, angle));
        }
    }
    c
}

macro_rules! snapshot_pipeline {
    ($name:ident, $circuit:expr) => {
        #[test]
        fn $name() {
            let source = $circuit;
            insta::assert_debug_snapshot!(concat!(stringify!($name), "_source"), source);

            let ir_opt = optimize_ir(&source);
            insta::assert_debug_snapshot!(concat!(stringify!($name), "_optimize_ir"), ir_opt);

            // Native decompose+optimize is its own parallel view (what
            // `decompositions.rs`/`verify_equivalence.rs`'s `run_native`
            // simulates against), not an input to `lower` — `lower`
            // takes the source-level `Circuit` and does its own
            // decompose+route internally. Snapshotting both views is
            // still useful: this one shows peephole-optimized native
            // gates in isolation, independent of any backend's routing.
            let native = decompose(&ir_opt);
            let native_opt = optimize(&native);
            insta::assert_debug_snapshot!(
                concat!(stringify!($name), "_native_optimized"),
                native_opt
            );

            for (label, backend) in [
                ("trapped_ion", Backend::TrappedIon),
                ("ibmq", Backend::IbmQ),
                ("rigetti", Backend::Rigetti),
            ] {
                let lowered = lower(&ir_opt, backend);
                insta::assert_debug_snapshot!(
                    format!("{}_{}", stringify!($name), label),
                    lowered
                );
            }
        }
    };
}

snapshot_pipeline!(bell_state, bell());
snapshot_pipeline!(ghz_4, ghz(4));
snapshot_pipeline!(qft_4, qft(4));
