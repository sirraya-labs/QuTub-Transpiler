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

--- Dump a real backend's coupling map + basis gates + live calibration ---
(feeds the Rust side: coupling.json's "edges"/"num_qubits" ->
CouplingMap::from_edges, "basis_gates" -> ibm_export::validate_cx_native_basis,
"single_qubit_fidelity"/"two_qubit_fidelity" -> a fresh PublishedCalibration
instead of fidelity.rs's fixed published snapshot)
    export IBM_QUANTUM_TOKEN=...
    export IBM_QUANTUM_INSTANCE=...
    python3 submit_ibm.py --dump-coupling-map --backend ibm_backend_name --out coupling.json

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


def dump_coupling_map(backend_name: str, out_path: str) -> None:
    """Queries a real backend's own coupling map, basis gates, and live
    gate-error calibration via Qiskit, and writes them to `out_path` as
    JSON for the Rust side to consume:

    - "edges" / "num_qubits" -> CouplingMap::from_edges (coupling.rs),
      to route against instead of CouplingMap::heavy_hex_for's
      synthetic topology (see backend::lower_with_coupling).
    - "basis_gates" -> ibm_export::validate_cx_native_basis, to catch
      an ECR-native device before to_ibm_qasm silently emits `cx`
      instructions it can't run.
    - "single_qubit_fidelity" / "two_qubit_fidelity" -> a fresh
      PublishedCalibration built from *today's* calibration, instead of
      fidelity.rs's fixed published snapshot (Priority 2 from the
      review this function's docstring above references).

    This does not submit anything; it's a read-only query, safe to run
    without spending shots.
    """
    from qiskit_ibm_runtime import QiskitRuntimeService
    import statistics

    token = os.environ.get("IBM_QUANTUM_TOKEN")
    instance = os.environ.get("IBM_QUANTUM_INSTANCE")
    if not token:
        sys.exit("IBM_QUANTUM_TOKEN is not set -- get one from your IBM Quantum account settings.")

    service = QiskitRuntimeService(channel="ibm_quantum_platform", token=token, instance=instance)
    backend = (
        service.backend(backend_name)
        if backend_name
        else service.least_busy(operational=True, simulator=False)
    )
    print(f"Querying backend: {backend.name}", file=sys.stderr)

    target = backend.target
    num_qubits = backend.num_qubits

    # Real, possibly-irregular coupling edges: every 2-qubit instruction
    # entry in the target, not a topology family this crate assumes.
    # Real devices retire/disable individual qubits and edges, so this
    # is read straight off the target rather than re-derived from a
    # regular lattice shape.
    edges = set()
    for gate_name in target.operation_names:
        for qargs in target[gate_name]:
            if qargs is not None and len(qargs) == 2:
                edges.add(tuple(sorted(qargs)))

    basis_gates = sorted(target.operation_names)

    # This device's own native single-/two-qubit gates, not assumed to
    # be 'sx'/'cx' -- an ECR-native device has no 'cx' entry at all.
    single_q_gate = "sx" if "sx" in target.operation_names else None
    two_q_gate = (
        "cx" if "cx" in target.operation_names
        else "ecr" if "ecr" in target.operation_names
        else None
    )

    def avg_error(gate_name):
        if gate_name is None:
            return None
        errors = [
            props.error
            for props in target[gate_name].values()
            if props is not None and props.error is not None
        ]
        return statistics.mean(errors) if errors else None

    single_q_error = avg_error(single_q_gate)
    two_q_error = avg_error(two_q_gate)

    data = {
        "backend_name": backend.name,
        "num_qubits": num_qubits,
        "edges": sorted(list(e) for e in edges),
        "basis_gates": basis_gates,
        "native_single_qubit_gate": single_q_gate,
        "native_two_qubit_gate": two_q_gate,
        "single_qubit_fidelity": (1.0 - single_q_error) if single_q_error is not None else None,
        "two_qubit_fidelity": (1.0 - two_q_error) if two_q_error is not None else None,
    }

    with open(out_path, "w") as f:
        json.dump(data, f, indent=2)

    print(
        f"Wrote {out_path}: {num_qubits} qubits, {len(edges)} coupling edges, "
        f"basis {basis_gates}",
        file=sys.stderr,
    )
    if two_q_gate == "ecr":
        print(
            "WARNING: this backend is ECR-native, not CX-native. ibm_export.rs's "
            "to_ibm_qasm assumes a CX-native basis and will emit invalid QASM for "
            "this device -- see ibm_export::validate_cx_native_basis, which will "
            "reject this basis_gates list until an ECR export path exists.",
            file=sys.stderr,
        )
    elif two_q_gate is None:
        print(
            "WARNING: could not identify this backend's native two-qubit gate from "
            "its basis_gates -- do not assume CX-native without checking.",
            file=sys.stderr,
        )


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
    parser.add_argument("--qasm", help="Path to QASM file from ibm_export::to_ibm_qasm")
    parser.add_argument("--shots", type=int, default=4096)
    parser.add_argument("--backend", default=None, help="IBM backend name (real hardware only)")
    parser.add_argument("--real", action="store_true", help="Submit to real IBM hardware instead of a local simulator")
    parser.add_argument("--compare", default=None, help="Path to a reference counts JSON (e.g. dumped from the Rust simulator run) to diff against")
    parser.add_argument(
        "--dump-coupling-map",
        action="store_true",
        help="Query --backend's real coupling map, basis gates, and live calibration and "
             "write them to --out as JSON, instead of submitting a circuit. Requires "
             "IBM_QUANTUM_TOKEN (and usually IBM_QUANTUM_INSTANCE).",
    )
    parser.add_argument("--out", default="coupling.json", help="Output path for --dump-coupling-map")
    args = parser.parse_args()

    if args.dump_coupling_map:
        dump_coupling_map(args.backend, args.out)
        return

    if not args.qasm:
        parser.error("--qasm is required unless --dump-coupling-map is given")

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