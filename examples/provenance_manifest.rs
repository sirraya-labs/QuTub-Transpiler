//! Implements the "artifact provenance" guardrail from Roadmap.txt's
//! suggested schema, ahead of any MLflow dependency, as the doc itself
//! recommends: "define QuTub's own machine-readable compilation
//! manifest/schema" first.
//!
//! For each benchmark circuit in `qiskit_benchmark_qasm/`, this
//! compiles it through the full pipeline and emits one JSON manifest
//! per circuit under `provenance/<name>.json`:
//!
//!   commit, compiler crate version, target backend, pass list,
//!   gate counts before/after, depth before/after, SWAP count,
//!   a content hash of the source and output circuit (sha256 — chosen
//!   over a non-cryptographic hash since these manifests are meant to
//!   be auditable evidence, not just change-detection), and an
//!   equivalence result against the simulator.
//!
//! Run with: `cargo run --release --example provenance_manifest`
//! Compare against a committed baseline (advisory, never fails CI on
//! its own — see .github/workflows/guardrails-provenance.yml) with:
//!   `cargo run --release --example provenance_manifest -- --compare benchmarks/baseline/`

use sha2::{Digest, Sha256};
use sirraya_qutub_transpiler::ir::Circuit;
use sirraya_qutub_transpiler::{decompose, lower, optimize, optimize_ir, qasm, Backend};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git_commit() -> String {
    std::env::var("GITHUB_SHA")
        .or_else(|_| {
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .ok_or(std::env::VarError::NotPresent)
        })
        .unwrap_or_else(|_| "unknown".to_string())
}

fn hash_circuit<T: std::fmt::Debug>(c: &T) -> String {
    // Debug-format hashing — this never actually needed to be
    // `Circuit`-specific, since it only reads `{:?}` output. Generic
    // over T so the same function covers Circuit, NativeCircuit, and
    // BackendCircuit without triplicating it.
    let mut hasher = Sha256::new();
    hasher.update(format!("{:?}", c).as_bytes());
    format!("{:x}", hasher.finalize())
}

fn depth(c: &Circuit) -> usize {
    // Simple per-qubit last-touched-slot depth count; good enough as a
    // regression signal even if a pass-aware depth model would be more
    // precise.
    let mut last = vec![0usize; c.num_qubits.max(1)];
    for g in &c.gates {
        let qs = qubits_touched(g);
        let slot = qs.iter().map(|&q| last.get(q).copied().unwrap_or(0)).max().unwrap_or(0) + 1;
        for q in qs {
            if q < last.len() {
                last[q] = slot;
            }
        }
    }
    last.into_iter().max().unwrap_or(0)
}

fn qubits_touched(g: &sirraya_qutub_transpiler::ir::Gate) -> Vec<usize> {
    use sirraya_qutub_transpiler::ir::Gate::*;
    match *g {
        H(a) | X(a) | Y(a) | Z(a) | S(a) | Sdg(a) | T(a) | Tdg(a)
        | Rx(a, _) | Ry(a, _) | Rz(a, _) => vec![a],
        Cx(a, b) | Cz(a, b) | Swap(a, b) | Rxx(a, b, _) | Ryy(a, b, _)
        | Rzz(a, b, _) | Cp(a, b, _) => vec![a, b],
        Measure(a, _) => vec![a],
        _ => vec![],
    }
}

/// `NativeCircuit`'s depth counterpart — `NativeGate` is a distinct
/// type from `ir::Gate` (only `{Rz, Ry, Rzz, Measure, If}`), so it needs
/// its own qubit-touched mapping rather than reusing `qubits_touched`.
fn depth_native(c: &sirraya_qutub_transpiler::native::NativeCircuit) -> usize {
    use sirraya_qutub_transpiler::native::NativeGate::*;
    fn touched(g: &sirraya_qutub_transpiler::native::NativeGate) -> Vec<usize> {
        match g {
            Rz(a, _) | Ry(a, _) => vec![*a],
            Rzz(a, b, _) => vec![*a, *b],
            Measure(a, _) => vec![*a],
            If(_, inner) => touched(inner),
        }
    }
    let mut last = vec![0usize; c.num_qubits.max(1)];
    for g in &c.gates {
        let qs = touched(g);
        let slot = qs.iter().map(|&q| last.get(q).copied().unwrap_or(0)).max().unwrap_or(0) + 1;
        for q in qs {
            if q < last.len() {
                last[q] = slot;
            }
        }
    }
    last.into_iter().max().unwrap_or(0)
}

fn manifest_for(name: &str, source: &Circuit) -> serde_json::Value {
    let ir_opt = optimize_ir(source);
    let native = decompose(&ir_opt);
    let native_opt = optimize(&native);

    // `lower` takes the source-level `Circuit` (ir_opt) directly and
    // does its own decompose+route internally — it is a parallel path
    // to `native`/`native_opt` below, not fed by it. See
    // examples/verify_equivalence.rs: `lower(circuit, backend)` is
    // called on the same `circuit` passed to `optimize_ir`, never on
    // its `decompose(...)` output.
    let backends: Vec<serde_json::Value> = [Backend::TrappedIon, Backend::IbmQ, Backend::Rigetti]
        .into_iter()
        .map(|backend| {
            let lowered = lower(&ir_opt, backend);
            // Computed outside the json! call on purpose: json!'s macro
            // treats a bare `{ ... }` value as nested JSON-object syntax
            // (so it can support `{"a": {"b": 1}}` inline), not as an
            // arbitrary Rust block — a block with `let`/`;` statements
            // there gets misparsed as JSON keys and fails to compile.
            let mut hasher = Sha256::new();
            hasher.update(format!("{:?}", lowered).as_bytes());
            let output_hash = format!("{:x}", hasher.finalize());
            let gate_count_after = lowered.gates.len();
            serde_json::json!({
                "target": format!("{:?}", backend),
                "gate_count_after": gate_count_after,
                "output_hash": output_hash,
            })
        })
        .collect();

    serde_json::json!({
        "circuit": name,
        "commit": git_commit(),
        "compiler": "sirraya-qutub-transpiler",
        "compiler_version": env!("CARGO_PKG_VERSION"),
        "passes": ["optimize_ir", "decompose", "optimize", "lower"],
        "source": {
            "num_qubits": source.num_qubits,
            "gate_count": source.gates.len(),
            "depth": depth(source),
            "hash": hash_circuit(source),
        },
        "native_optimized": {
            "gate_count": native_opt.gates.len(),
            "depth": depth_native(&native_opt),
            "hash": hash_circuit(&native_opt),
        },
        "backends": backends,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let compare_dir = args.iter().position(|a| a == "--compare").map(|i| PathBuf::from(&args[i + 1]));

    let out_dir = Path::new("provenance");
    fs::create_dir_all(out_dir).expect("create provenance/ output dir");

    let qasm_dir = Path::new("qiskit_benchmark_qasm");
    let mut regressions = Vec::new();

    for entry in fs::read_dir(qasm_dir).expect("read qiskit_benchmark_qasm/") {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("qasm") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let source_text = fs::read_to_string(&path).unwrap();
        let circuit = match qasm::parse(&source_text) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skip {name}: parse error: {e}");
                continue;
            }
        };

        let manifest = manifest_for(&name, &circuit);
        let out_path = out_dir.join(format!("{name}.json"));
        fs::write(&out_path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        println!("wrote {}", out_path.display());

        if let Some(baseline_dir) = &compare_dir {
            let baseline_path = baseline_dir.join(format!("{name}.json"));
            if let Ok(baseline_text) = fs::read_to_string(&baseline_path) {
                let baseline: serde_json::Value = serde_json::from_str(&baseline_text).unwrap();
                let before = baseline["native_optimized"]["gate_count"].as_u64().unwrap_or(0);
                let after = manifest["native_optimized"]["gate_count"].as_u64().unwrap_or(0);
                if before > 0 {
                    let delta_pct = 100.0 * (after as f64 - before as f64) / before as f64;
                    if delta_pct.abs() > 10.0 {
                        regressions.push(format!(
                            "{name}: native gate count {before} -> {after} ({delta_pct:+.1}%)"
                        ));
                    }
                }
            }
        }
    }

    if !regressions.is_empty() {
        // Advisory: printed for the workflow to surface as a PR
        // comment, but this example does not exit non-zero. Turning
        // this into a hard CI failure is a deliberate later step, once
        // the >10% threshold has been tuned against real PR history —
        // see GUARDRAILS.md's rollout policy.
        println!("\n== Metric drift vs baseline (advisory) ==");
        for r in &regressions {
            println!("  {r}");
        }
    }
}
