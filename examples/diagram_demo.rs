use sirraya_qutub_transpiler::backend::{lower, Backend};
use sirraya_qutub_transpiler::diagram::Diagram;
use sirraya_qutub_transpiler::ir::{Circuit, Gate};
use sirraya_qutub_transpiler::native::decompose;

fn main() {
    let mut c = Circuit::new(3);
    c.num_clbits = 1;
    c.push(Gate::H(0))
        .push(Gate::Cx(0, 2))
        .push(Gate::Rz(1, 0.5))
        .push(Gate::Swap(1, 2))
        .push(Gate::Measure(0, 0));

    println!("=== source (ir::Circuit) ===");
    println!("{}", Diagram::from_circuit(&c).to_ascii());

    println!("\n=== native ({{Rz,Ry,Rzz}}) ===");
    println!("{}", Diagram::from_native(&decompose(&c)).to_ascii());

    println!("\n=== backend-lowered (IbmQ) ===");
    let bc = lower(&c, Backend::IbmQ);
    println!("{}", Diagram::from_backend(&bc).to_ascii());

    let svg = Diagram::from_circuit(&c).to_svg();
    let out_path = "diagram.svg";
    std::fs::write(out_path, &svg).expect("failed to write SVG file");
    println!("\n=== SVG (source) written to {} ({} bytes) ===", out_path, svg.len());
}