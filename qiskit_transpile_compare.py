#!/usr/bin/env python3
"""
Loads the same benchmark circuits `qiskit_benchmark.rs` wrote to
./qiskit_benchmark_qasm/*.qasm and transpiles each with Qiskit's own
transpiler, targeting the same real IBM basis
(`{rz, sx, x, cx}` -- see `ibm_export.rs`'s module doc for why this,
specifically, is IBM hardware's actual native gate set) *and* the same
heavy-hex connectivity constraint (`{name}_coupling.txt`, the exact
`CouplingMap::heavy_hex_for(n)` topology `backend::lower(Backend::IbmQ)`
routed each benchmark against) that
`sirraya_qutub_transpiler`'s own pipeline targets.

The coupling-map constraint matters: without it, Qiskit is transpiling
against unconstrained all-to-all connectivity, which is a materially
easier problem than the real, sparse hardware topology this crate
routes against -- a large gate-count gap under that mismatched
comparison would be measuring "no routing needed" vs. "real routing
needed," not transpiler quality. With a matching coupling map, both
sides are solving the identical constrained problem.

Run with:
    cargo run --example qiskit_benchmark   # writes the QASM + coupling files first
    python3 qiskit_transpile_compare.py

Requires: pip install qiskit
"""
import glob
import os
import sys


def load_coupling_map(path):
    """Reads an `i j` edge-list file and returns a Qiskit CouplingMap
    with edges in both directions (Qiskit's CouplingMap is directed;
    a physical two-qubit gate here is undirected, so both directions
    need to be present for transpile() to route freely across it)."""
    from qiskit.transpiler import CouplingMap

    edges = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            i, j = (int(x) for x in line.split())
            edges.append([i, j])
            edges.append([j, i])
    return CouplingMap(couplinglist=edges)


def main():
    try:
        from qiskit import QuantumCircuit, transpile
    except ImportError:
        sys.exit("This script requires Qiskit: pip install qiskit")

    qasm_dir = "qiskit_benchmark_qasm"
    files = sorted(glob.glob(os.path.join(qasm_dir, "*.qasm")))
    if not files:
        sys.exit(
            f"No QASM files found in ./{qasm_dir}/ -- run "
            "`cargo run --example qiskit_benchmark` first to generate them."
        )

    basis_gates = ["rz", "sx", "x", "cx"]  # IBM's real native gate set

    print(f"{'benchmark':<28}  {'src gates':>10}  {'depth':>6}  {'basis gates':>12}  {'2q gates':>10}")
    for path in files:
        name = os.path.splitext(os.path.basename(path))[0]
        coupling_path = os.path.join(qasm_dir, f"{name}_coupling.txt")
        if not os.path.exists(coupling_path):
            sys.exit(
                f"Missing {coupling_path} -- re-run `cargo run --example qiskit_benchmark` "
                "with the version that exports coupling maps."
            )

        with open(path) as f:
            qasm_text = f.read()

        circuit = QuantumCircuit.from_qasm_str(qasm_text)
        src_gate_count = sum(circuit.count_ops().values())
        coupling_map = load_coupling_map(coupling_path)

        transpiled = transpile(
            circuit,
            basis_gates=basis_gates,
            coupling_map=coupling_map,
            optimization_level=3,
        )
        counts = transpiled.count_ops()
        total_basis_gates = sum(counts.values())
        two_qubit_gates = counts.get("cx", 0)

        print(
            f"{name:<28}  {src_gate_count:>10}  {transpiled.depth():>6}  "
            f"{total_basis_gates:>12}  {two_qubit_gates:>10}"
        )

    print(
        "\nCompare this table's 'basis gates' / '2q gates' columns directly against "
        "qiskit_benchmark.rs's '1q (IBM)' / '2q (IBM)' columns for the same benchmark "
        "name -- both are gate counts in the identical {rz, sx, x, cx} target basis, "
        "routed against the identical heavy-hex coupling map, for the identical source "
        "circuit."
    )


if __name__ == "__main__":
    main()