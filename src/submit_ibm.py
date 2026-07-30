#!/usr/bin/env python3
"""
Runs a QASM 2.0 circuit (as exported by ibm_export::to_ibm_qasm on the
Rust side) either on a local simulator or on real IBM Quantum hardware,
and prints the resulting measurement counts as JSON.

There is no official Rust SDK for IBM Quantum Platform / Qiskit
Runtime, so this is the intended bridge: the Rust crate produces real
IBM-basis QASM text, and this script is what actually submits it.

--- Local sanity check (no IBM account needed) ---
    python3 submit_ibm.py --qasm bell.qasm --shots 4096

--- Real hardware ---
    export IBM_QUANTUM_TOKEN=...       # from quantum.ibm.com account settings
    export IBM_QUANTUM_INSTANCE=...    # CRN of your instance/plan
    python3 submit_ibm.py --qasm bell.qasm --shots 4096 --backend ibm_backend_name --real

Requires: pip install qiskit qiskit-ibm-runtime
(qiskit alone is enough for --local, which is the default)
"""
import argparse
import json
import os
import sys


def load_circuit(qasm_path: str):
    from qiskit import QuantumCircuit

    with open(qasm_path, "r") as f:
        qasm_text = f.read()
    # qiskit's QASM2 loader wants the real 'qelib1.inc' dialect this
    # script's Rust counterpart (ibm_export::to_ibm_qasm) emits --
    # rz/sx/x/cx/measure -- not sirraya_qutub's own rz/ry/rzz dialect
    # (emit::to_qasm). Don't feed that one in here; it will parse but
    # produce the wrong circuit, since 'ry'/'rzz' aren't IBM's basis.
    return QuantumCircuit.from_qasm_str(qasm_text)


def run_local(circuit, shots: int) -> dict:
    """Local sanity check: confirms the exported QASM parses and runs
    at all, with no IBM account needed. This does NOT validate
    real-device noise or connectivity -- only that the plumbing
    (Rust export -> QASM -> Qiskit -> a result) works end to end."""
    from qiskit_aer import AerSimulator

    sim = AerSimulator()
    job = sim.run(circuit, shots=shots)
    counts = job.result().get_counts()
    return dict(counts)


def run_real(circuit, shots: int, backend_name: str) -> dict:
    from qiskit_ibm_runtime import QiskitRuntimeService, SamplerV2

    token = os.environ.get("IBM_QUANTUM_TOKEN")
    instance = os.environ.get("IBM_QUANTUM_INSTANCE")
    if not token:
        sys.exit("IBM_QUANTUM_TOKEN is not set -- get one from your IBM Quantum account settings.")

    service = QiskitRuntimeService(channel="ibm_quantum_platform", token=token, instance=instance)
    backend = service.backend(backend_name) if backend_name else service.least_busy(operational=True, simulator=False)
    print(f"Submitting to real backend: {backend.name}", file=sys.stderr)

    # optimization_level=0: routing and native-gate lowering already
    # happened on the Rust side (backend::lower + ibm_export). Letting
    # Qiskit re-transpile here would mean you're testing Qiskit's
    # transpiler output, not this crate's.
    sampler = SamplerV2(mode=backend)
    job = sampler.run([circuit], shots=shots)
    print(f"Job ID: {job.job_id()}", file=sys.stderr)
    result = job.result()
    counts = result[0].data.c.get_counts()
    return dict(counts)


def total_variation_distance(a: dict, b: dict) -> float:
    """Compares two bitstring-count distributions after normalizing to
    probabilities. 0.0 = identical distributions, 1.0 = fully disjoint.
    Real hardware will never match a simulator exactly -- this is the
    metric for 'close to the ideal distribution', not 'identical'."""
    keys = set(a) | set(b)
    total_a = sum(a.values()) or 1
    total_b = sum(b.values()) or 1
    return 0.5 * sum(abs(a.get(k, 0) / total_a - b.get(k, 0) / total_b) for k in keys)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--qasm", required=True, help="Path to QASM file from ibm_export::to_ibm_qasm")
    parser.add_argument("--shots", type=int, default=4096)
    parser.add_argument("--backend", default=None, help="IBM backend name (real hardware only)")
    parser.add_argument("--real", action="store_true", help="Submit to real IBM hardware instead of a local simulator")
    parser.add_argument("--compare", default=None, help="Path to a reference counts JSON (e.g. dumped from the Rust simulator run) to diff against")
    args = parser.parse_args()

    circuit = load_circuit(args.qasm)
    counts = run_real(circuit, args.shots, args.backend) if args.real else run_local(circuit, args.shots)

    print(json.dumps(counts, indent=2))

    if args.compare:
        with open(args.compare) as f:
            reference = json.load(f)
        tvd = total_variation_distance(counts, reference)
        print(f"\nTotal variation distance vs {args.compare}: {tvd:.4f}", file=sys.stderr)


if __name__ == "__main__":
    main()
