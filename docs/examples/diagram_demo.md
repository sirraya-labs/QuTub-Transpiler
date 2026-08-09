# diagram_demo

Watch a circuit change shape as it moves through compilation, instead of just reading gate counts.

```bash
cargo run --example diagram_demo
```

## What it does

Builds a small 3-qubit circuit (`H`, `Cx`, `Rz`, `Swap`, `Measure`) and renders it three times — as the original source IR, after native `{Rz, Ry, Rzz}` decomposition, and after lowering to IBM's backend — using `Diagram::from_circuit` / `from_native` / `from_backend`, each producing ASCII art. It also writes an SVG rendering of the source circuit to disk.

```rust
println!("{}", Diagram::from_circuit(&c).to_ascii());
println!("{}", Diagram::from_native(&decompose(&c)).to_ascii());
let bc = lower(&c, Backend::IbmQ);
println!("{}", Diagram::from_backend(&bc).to_ascii());

let svg = Diagram::from_circuit(&c).to_svg();
std::fs::write("diagram.svg", &svg).expect("failed to write SVG file");
```

## Output

Three ASCII diagrams printed to the terminal, plus `diagram.svg` written to the working directory — usable directly in documentation or a paper figure, not just a debugging aid.

## Why it matters

Gate counts tell you *how much* changed; a diagram tells you *what* changed and *where*. Useful any time you're debugging why a rewrite pass did something unexpected, or when writing documentation that needs to show a circuit rather than describe it in prose.

## Related

- [`pipeline_end_to_end`](pipeline_end_to_end.md) — uses the same `Diagram` API on a larger, multi-stage pipeline
- [`routing_demo`](routing_demo.md) — shows a before/after diagram specifically to make SWAP insertion visible
