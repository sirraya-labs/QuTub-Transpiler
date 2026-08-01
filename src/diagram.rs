//! Renders a circuit -- at any of the three levels this crate's
//! pipeline produces ([`crate::ir::Circuit`], [`crate::native::NativeCircuit`],
//! or [`crate::backend::BackendCircuit`]) -- as a standard quantum
//! circuit diagram, either as ASCII text (a `String` you can print) or
//! as a standalone SVG document (a `String` of valid XML).
//!
//! # Design
//! All three source gate sets funnel into one shared intermediate model,
//! [`Diagram`]/[`DiagramInstr`], so there's exactly one column-packing
//! algorithm and exactly one pair of renderers (`to_ascii`, `to_svg`),
//! not three of each. [`DiagramInstr`] only needs five shapes to cover
//! every gate this crate has at any level: a single-qubit box, a
//! controlled gate (one or more control qubits plus a target that's
//! either boxed or itself a plain control dot -- see `Cz`), a
//! two-qubit "spanning" box (`Rxx`/`Ryy`/`Rzz`), a `Swap` marker, and a
//! `Measure` marker.
//!
//! # Column packing
//! Gates are laid out into as few columns as their qubit ranges allow
//! (a classic greedy interval-packing pass, [`assign_columns`]): two
//! gates share a column whenever their qubit *ranges* don't overlap --
//! not just their exact qubit sets, since a control/target or spanning
//! gate's connecting line visually occupies every wire strictly between
//! its endpoints too, whether or not that wire is one of its arguments.
//! This is what keeps a `Cx(0, 4)` from silently overlapping something
//! drawn on qubit 2 in the same column.
//!
//! # ASCII rendering
//! Pure ASCII on purpose (no Unicode box-drawing) -- `-` for wires,
//! `|` for vertical connectors, `*` for control dots, `[label]` for
//! boxes, `X` for swap markers. Every column is padded to the width its
//! widest cell needs, so columns line up exactly across every wire row.
//!
//! # SVG rendering
//! A fixed-size grid (one row per qubit, one fixed-width column per
//! diagram column), rendered as plain SVG primitives (`<rect>`,
//! `<circle>`, `<line>`, `<text>`) -- a real, valid, standalone SVG
//! document string, not a `crate::visualize`-style embedded widget
//! (this is a library function with no chat/tool dependency).

use crate::backend::{BackendCircuit, BackendGate, RotAxis};
use crate::ir::{Circuit, Gate};
use crate::native::{NativeCircuit, NativeGate};

/// One instruction's shape, in the shared diagram model. Every variant
/// carries only wire *indices* (already whatever numbering the source
/// circuit used -- logical for [`Diagram::from_circuit`], physical for
/// [`Diagram::from_backend`]) and pre-formatted display labels; nothing
/// here needs to know which of the three source gate sets it came from.
#[derive(Debug, Clone, PartialEq)]
pub enum DiagramInstr {
    /// A single boxed label on one wire, e.g. `H`, `RZ(0.30)`.
    Single { qubit: usize, label: String },
    /// One or more control dots plus a target. `target_label` is the
    /// boxed label to draw on the target (`Cx` -> `"X"`, `Cp` -> a
    /// formatted `"P(...)"`); `None` draws a plain control dot on the
    /// target too, matching the conventional two-dots-and-a-line style
    /// for a gate like `Cz` that's symmetric in its two qubits.
    Controlled {
        controls: Vec<usize>,
        target: usize,
        target_label: Option<String>,
    },
    /// A single box spanning both wires with one shared, centered
    /// label -- `Rxx`/`Ryy`/`Rzz`'s conventional rendering, since
    /// neither qubit plays a distinguished "control" or "target" role.
    Span { qubits: (usize, usize), label: String },
    /// A `Swap`, drawn as an `X` marker on each of the two wires,
    /// connected by a vertical line.
    Swap { a: usize, b: usize },
    /// A measurement of `qubit` into classical bit `clbit`. This
    /// crate's diagram model doesn't draw a separate classical-wire
    /// row (none of the three circuit levels track one) -- the target
    /// clbit is folded into the marker's own label instead.
    Measure { qubit: usize, clbit: usize },
}

impl DiagramInstr {
    /// The inclusive `(min, max)` wire range this instruction's box or
    /// connecting line visually occupies -- used both for column
    /// packing (see the module doc) and for rendering, since every
    /// wire strictly between the endpoints needs a passthrough marker.
    fn wire_range(&self) -> (usize, usize) {
        match self {
            DiagramInstr::Single { qubit, .. } => (*qubit, *qubit),
            DiagramInstr::Measure { qubit, .. } => (*qubit, *qubit),
            DiagramInstr::Controlled { controls, target, .. } => {
                let mut lo = *target;
                let mut hi = *target;
                for &c in controls {
                    lo = lo.min(c);
                    hi = hi.max(c);
                }
                (lo, hi)
            }
            DiagramInstr::Span { qubits: (a, b), .. } => {
                (*a.min(b), *a.max(b))
            }
            DiagramInstr::Swap { a, b } => (*a.min(b), *a.max(b)),
        }
    }
}

/// A circuit ready to be rendered, in the shared diagram model. Build
/// one via [`Diagram::from_circuit`], [`Diagram::from_native`], or
/// [`Diagram::from_backend`], then call [`Diagram::to_ascii`] or
/// [`Diagram::to_svg`].
#[derive(Debug, Clone)]
pub struct Diagram {
    pub num_qubits: usize,
    pub num_clbits: usize,
    pub instrs: Vec<DiagramInstr>,
    /// Per-wire display label, e.g. `"q0"` -- always `num_qubits` long.
    /// Broken out (rather than hardcoded `"q{n}"` in the renderers) so
    /// a future caller could label physical wires differently from
    /// logical ones without touching either renderer.
    pub wire_labels: Vec<String>,
}

fn default_wire_labels(num_qubits: usize) -> Vec<String> {
    (0..num_qubits).map(|q| format!("q{}", q)).collect()
}

fn fmt_angle(label: &str, angle: f64) -> String {
    format!("{}({:.2})", label, angle)
}

impl Diagram {
    /// Builds a diagram from a source-level [`Circuit`] (logical
    /// qubits, pre-routing) -- the richest of the three gate sets, so
    /// this is the only conversion that needs every [`DiagramInstr`]
    /// variant.
    pub fn from_circuit(circuit: &Circuit) -> Self {
        let mut instrs = Vec::with_capacity(circuit.gates.len());
        for gate in &circuit.gates {
            let instr = match *gate {
                Gate::H(q) => single(q, "H"),
                Gate::X(q) => single(q, "X"),
                Gate::Y(q) => single(q, "Y"),
                Gate::Z(q) => single(q, "Z"),
                Gate::S(q) => single(q, "S"),
                Gate::Sdg(q) => single(q, "SDG"),
                Gate::T(q) => single(q, "T"),
                Gate::Tdg(q) => single(q, "TDG"),
                Gate::Rx(q, a) => DiagramInstr::Single { qubit: q, label: fmt_angle("RX", a) },
                Gate::Ry(q, a) => DiagramInstr::Single { qubit: q, label: fmt_angle("RY", a) },
                Gate::Rz(q, a) => DiagramInstr::Single { qubit: q, label: fmt_angle("RZ", a) },
                Gate::Cx(c, t) => controlled(c, t, Some("X".to_string())),
                Gate::Cz(a, b) => controlled(a, b, None),
                Gate::Swap(a, b) => DiagramInstr::Swap { a, b },
                Gate::Rxx(a, b, t) => DiagramInstr::Span { qubits: (a, b), label: fmt_angle("RXX", t) },
                Gate::Ryy(a, b, t) => DiagramInstr::Span { qubits: (a, b), label: fmt_angle("RYY", t) },
                Gate::Rzz(a, b, t) => DiagramInstr::Span { qubits: (a, b), label: fmt_angle("RZZ", t) },
                Gate::Cp(c, t, l) => controlled(c, t, Some(fmt_angle("P", l))),
                Gate::Measure(q, c) => DiagramInstr::Measure { qubit: q, clbit: c },
            };
            instrs.push(instr);
        }
        Self {
            num_qubits: circuit.num_qubits,
            num_clbits: circuit.num_clbits,
            instrs,
            wire_labels: default_wire_labels(circuit.num_qubits),
        }
    }

    /// Builds a diagram from a [`NativeCircuit`] (the decomposed
    /// `{Rz, Ry, Rzz}` gate set) -- three gate kinds map to exactly
    /// three [`DiagramInstr`] shapes.
    pub fn from_native(circuit: &NativeCircuit) -> Self {
        let mut instrs = Vec::with_capacity(circuit.gates.len());
        for gate in &circuit.gates {
            let instr = match *gate {
                NativeGate::Rz(q, a) => DiagramInstr::Single { qubit: q, label: fmt_angle("RZ", a) },
                NativeGate::Ry(q, a) => DiagramInstr::Single { qubit: q, label: fmt_angle("RY", a) },
                NativeGate::Rzz(a, b, t) => {
                    DiagramInstr::Span { qubits: (a, b), label: fmt_angle("RZZ", t) }
                }
                NativeGate::Measure(q, c) => DiagramInstr::Measure { qubit: q, clbit: c },
            };
            instrs.push(instr);
        }
        Self {
            num_qubits: circuit.num_qubits,
            num_clbits: circuit.num_clbits,
            instrs,
            wire_labels: default_wire_labels(circuit.num_qubits),
        }
    }

    /// Builds a diagram from a backend-lowered [`BackendCircuit`]
    /// (physical qubits, post-routing). `BackendGate::Rot`'s label
    /// depends on which backend the circuit was lowered for --
    /// `RY` for `TrappedIon`, `RX` for `IbmQ`/`Rigetti` -- matching
    /// `emit::apply_backend_to`'s own per-backend interpretation of
    /// that gate (see `backend`'s module doc), so the diagram's labels
    /// never silently disagree with what the circuit actually executes
    /// as.
    pub fn from_backend(circuit: &BackendCircuit) -> Self {
        let rot_axis = match circuit.backend.rot_axis() {
            RotAxis::Ry => "RY",
            RotAxis::Rx => "RX",
        };
        let mut instrs = Vec::with_capacity(circuit.gates.len());
        for gate in &circuit.gates {
            let instr = match *gate {
                BackendGate::Rz(q, a) => DiagramInstr::Single { qubit: q, label: fmt_angle("RZ", a) },
                BackendGate::Rot(q, a) => {
                    DiagramInstr::Single { qubit: q, label: fmt_angle(rot_axis, a) }
                }
                BackendGate::Cx(a, b) => controlled(a, b, Some("X".to_string())),
                BackendGate::Cz(a, b) => controlled(a, b, None),
                BackendGate::Rzz(a, b, t) => {
                    DiagramInstr::Span { qubits: (a, b), label: fmt_angle("RZZ", t) }
                }
                BackendGate::Measure(q, c) => DiagramInstr::Measure { qubit: q, clbit: c },
            };
            instrs.push(instr);
        }
        Self {
            num_qubits: circuit.num_qubits,
            num_clbits: circuit.num_clbits,
            instrs,
            wire_labels: default_wire_labels(circuit.num_qubits),
        }
    }

    /// Renders as ASCII text (see this module's doc comment for the
    /// glyph set and why it's plain ASCII, not Unicode box-drawing).
    pub fn to_ascii(&self) -> String {
        crate::diagram::ascii::render(self)
    }

    /// Renders as a standalone SVG document (valid XML, ready to write
    /// to a `.svg` file or embed inline).
    pub fn to_svg(&self) -> String {
        crate::diagram::svg::render(self)
    }
}

fn single(qubit: usize, label: &str) -> DiagramInstr {
    DiagramInstr::Single { qubit, label: label.to_string() }
}

fn controlled(control: usize, target: usize, target_label: Option<String>) -> DiagramInstr {
    DiagramInstr::Controlled { controls: vec![control], target, target_label }
}

/// Greedily packs `instrs` into as few columns as possible: instruction
/// `i` goes in the first column where its [`DiagramInstr::wire_range`]
/// doesn't overlap any instruction already placed in that column, for
/// *any* wire in its range (not just the wires it directly acts on --
/// see the module doc for why). Returns one column index per
/// instruction, in the same order as `instrs`.
fn assign_columns(instrs: &[DiagramInstr], num_qubits: usize) -> Vec<usize> {
    let mut next_free_col = vec![0usize; num_qubits.max(1)];
    let mut cols = Vec::with_capacity(instrs.len());
    for instr in instrs {
        let (lo, hi) = instr.wire_range();
        let col = (lo..=hi).map(|w| next_free_col[w]).max().unwrap_or(0);
        for w in lo..=hi {
            next_free_col[w] = col + 1;
        }
        cols.push(col);
    }
    cols
}

/// Groups `instrs` by the column [`assign_columns`] gave each one,
/// returning one `Vec` per column (empty columns don't occur, since
/// `assign_columns` only ever produces the minimum column index that
/// has *something* in it up to that point... more precisely: this
/// just inverts the `instr -> column` map back into `column ->
/// [instrs]`, in increasing column order).
fn group_by_column<'a>(instrs: &'a [DiagramInstr], cols: &[usize]) -> Vec<Vec<&'a DiagramInstr>> {
    let num_cols = cols.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    let mut grouped: Vec<Vec<&DiagramInstr>> = vec![Vec::new(); num_cols];
    for (instr, &c) in instrs.iter().zip(cols) {
        grouped[c].push(instr);
    }
    grouped
}

mod ascii {
    use super::{assign_columns, group_by_column, Diagram, DiagramInstr};

    /// The content this instruction draws on `row`, or `None` if `row`
    /// isn't touched by it at all (the caller only calls this for rows
    /// inside the instruction's `wire_range`, so `None` only happens
    /// for a passthrough row strictly between two endpoints -- callers
    /// render that as a plain vertical connector).
    fn cell_content(instr: &DiagramInstr, row: usize) -> Option<String> {
        match instr {
            DiagramInstr::Single { qubit, label } if *qubit == row => {
                Some(format!("[{}]", label))
            }
            DiagramInstr::Measure { qubit, clbit } if *qubit == row => {
                Some(format!("[M->c{}]", clbit))
            }
            DiagramInstr::Controlled { controls, target, target_label } => {
                if row == *target {
                    Some(match target_label {
                        // Mirrors the SVG renderer's ⊕ symbol for
                        // CNOT's target -- ASCII has no circle glyph,
                        // so "(+)" stands in for it; any other
                        // labeled target still gets a boxed label.
                        Some(l) if l == "X" => "(+)".to_string(),
                        Some(l) => format!("[{}]", l),
                        None => "*".to_string(),
                    })
                } else if controls.contains(&row) {
                    Some("*".to_string())
                } else {
                    None
                }
            }
            DiagramInstr::Span { qubits: (a, b), label } => {
                if row == *a || row == *b {
                    Some(format!("[{}]", label))
                } else {
                    None
                }
            }
            DiagramInstr::Swap { a, b } => {
                if row == *a || row == *b {
                    Some("X".to_string())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn pad_center(content: &str, width: usize, fill: char) -> String {
        let len = content.chars().count();
        if len >= width {
            return content.to_string();
        }
        let total_pad = width - len;
        let left = total_pad / 2;
        let right = total_pad - left;
        format!(
            "{}{}{}",
            fill.to_string().repeat(left),
            content,
            fill.to_string().repeat(right)
        )
    }

    pub fn render(diagram: &Diagram) -> String {
        let num_qubits = diagram.num_qubits;
        if num_qubits == 0 {
            return String::new();
        }
        let cols = assign_columns(&diagram.instrs, num_qubits);
        let columns = group_by_column(&diagram.instrs, &cols);

        // Uniform label gutter width, so every wire row starts at the
        // same horizontal offset regardless of label length.
        let label_width = diagram.wire_labels.iter().map(|l| l.len()).max().unwrap_or(1);

        // One content cell per (row, column); `None` means "this row
        // isn't in any instruction's range this column" -> a plain
        // continuous wire.
        let mut cells: Vec<Vec<Option<String>>> = vec![vec![None; columns.len()]; num_qubits];
        let mut col_widths: Vec<usize> = vec![1; columns.len()];

        for (c, instrs_here) in columns.iter().enumerate() {
            for instr in instrs_here {
                let (lo, hi) = instr.wire_range();
                for row in lo..=hi {
                    let content = cell_content(instr, row).unwrap_or_else(|| "|".to_string());
                    col_widths[c] = col_widths[c].max(content.chars().count());
                    cells[row][c] = Some(content);
                }
            }
        }

        let mut lines = Vec::with_capacity(num_qubits);
        for row in 0..num_qubits {
            let mut line = format!("{:>width$}: ", diagram.wire_labels[row], width = label_width);
            for c in 0..columns.len() {
                let width = col_widths[c];
                let content = cells[row][c].clone().unwrap_or_default();
                line.push_str(&pad_center(&content, width, '-'));
            }
            lines.push(line);
        }
        lines.join("\n")
    }
}

mod svg {
    use super::{group_by_column, Diagram, DiagramInstr};
    use std::fmt::Write as _;

    const ROW_HEIGHT: f64 = 60.0;
    const COL_WIDTH: f64 = 80.0;
    const LEFT_MARGIN: f64 = 70.0;
    const RIGHT_MARGIN: f64 = 30.0;
    const TOP_MARGIN: f64 = 30.0;
    const BOX_HALF_W: f64 = 30.0;
    const BOX_HALF_H: f64 = 16.0;
    const DOT_RADIUS: f64 = 5.0;
    /// Extra vertical gap separating the classical-wire block from the
    /// last qubit row, so the double lines read as a visually distinct
    /// section rather than just another qubit.
    const CLBIT_GAP: f64 = 20.0;
    /// Spacing between the two parallel lines that make up one
    /// classical (double-line) wire.
    const CLBIT_LINE_OFFSET: f64 = 2.0;

    fn row_y(row: usize) -> f64 {
        TOP_MARGIN + (row as f64 + 0.5) * ROW_HEIGHT
    }

    /// Y position of the `cb`-th classical wire, drawn as a block below
    /// all `num_qubits` qubit rows.
    fn row_y_clbit(cb: usize, num_qubits: usize) -> f64 {
        TOP_MARGIN + (num_qubits as f64 + cb as f64 + 0.5) * ROW_HEIGHT + CLBIT_GAP
    }

    fn col_x(col: usize) -> f64 {
        LEFT_MARGIN + (col as f64 + 0.5) * COL_WIDTH
    }

    /// Escapes the handful of characters that matter inside SVG text
    /// content/attributes for the labels this module ever produces
    /// (angles, gate names) -- not a general XML escaper, but every
    /// label here is built from `fmt_angle`/fixed gate names, so `&`,
    /// `<`, `>` are the only characters that could ever appear.
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    fn draw_box(out: &mut String, x: f64, y: f64, label: &str) {
        let _ = write!(
            out,
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="white" stroke="black" stroke-width="1.5"/>"#,
            x - BOX_HALF_W,
            y - BOX_HALF_H,
            BOX_HALF_W * 2.0,
            BOX_HALF_H * 2.0,
        );
        let _ = write!(
            out,
            r#"<text x="{:.1}" y="{:.1}" text-anchor="middle" dominant-baseline="middle">{}</text>"#,
            x,
            y,
            esc(label),
        );
    }

    fn draw_dot(out: &mut String, x: f64, y: f64) {
        let _ = write!(
            out,
            r#"<circle cx="{:.1}" cy="{:.1}" r="{:.1}" fill="black"/>"#,
            x, y, DOT_RADIUS
        );
    }

    fn draw_vline(out: &mut String, x: f64, y1: f64, y2: f64) {
        let _ = write!(
            out,
            r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>"#,
            x, y1, x, y2
        );
    }

    /// A classical-wire connector: two parallel vertical lines (the
    /// conventional "double line" marking a classical bit, as opposed
    /// to the single line used for quantum wires), running from a
    /// measurement box down to its classical row.
    fn draw_double_vline(out: &mut String, x: f64, y1: f64, y2: f64) {
        draw_vline(out, x - CLBIT_LINE_OFFSET, y1, y2);
        draw_vline(out, x + CLBIT_LINE_OFFSET, y1, y2);
    }

    /// The conventional ⊕ ("XOR target") symbol used for a CNOT's
    /// target qubit in standard circuit-diagram notation: an unfilled
    /// circle with a plus sign through it, drawn directly on the wire
    /// -- distinct from the boxed-label style used for every other
    /// gate, and from the plain control dot used for a `None` target
    /// (see `Cz`'s symmetric two-dot rendering).
    fn draw_target_symbol(out: &mut String, x: f64, y: f64) {
        let r = 16.0;
        let _ = write!(
            out,
            r#"<circle cx="{:.1}" cy="{:.1}" r="{:.1}" fill="white" stroke="black" stroke-width="1.5"/>"#,
            x, y, r
        );
        let _ = write!(
            out,
            r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>"#,
            x - r, y, x + r, y
        );
        let _ = write!(
            out,
            r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>"#,
            x, y - r, x, y + r
        );
    }

    fn draw_swap_marker(out: &mut String, x: f64, y: f64) {
        let r = 8.0;
        let _ = write!(
            out,
            r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>"#,
            x - r, y - r, x + r, y + r
        );
        let _ = write!(
            out,
            r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>"#,
            x - r, y + r, x + r, y - r
        );
    }

    /// Column assignment for the SVG renderer specifically. This
    /// can't reuse the shared [`super::assign_columns`] (which the
    /// ASCII renderer still uses unchanged): in SVG, a `Measure`
    /// draws a real double-line connector down to its classical row,
    /// and that connector visually passes through every qubit row
    /// below the measured one within its column. The shared packer
    /// only reserves a `Measure`'s own row (there's no classical wire
    /// in ASCII to protect), so a gate on a lower qubit could
    /// otherwise land in the same column and be drawn right through
    /// the connector line. Here, a `Measure` reserves every row from
    /// its own qubit down to the last qubit -- forcing anything below
    /// it to a later column -- while every other instruction keeps
    /// its normal [`DiagramInstr::wire_range`].
    pub(crate) fn assign_columns_svg(instrs: &[DiagramInstr], num_qubits: usize) -> Vec<usize> {
        let mut next_free_col = vec![0usize; num_qubits.max(1)];
        let mut cols = Vec::with_capacity(instrs.len());
        for instr in instrs {
            let (lo, hi) = match instr {
                DiagramInstr::Measure { qubit, .. } => (*qubit, num_qubits.saturating_sub(1)),
                other => other.wire_range(),
            };
            let col = (lo..=hi).map(|w| next_free_col[w]).max().unwrap_or(0);
            for w in lo..=hi {
                next_free_col[w] = col + 1;
            }
            cols.push(col);
        }
        cols
    }

    pub fn render(diagram: &Diagram) -> String {
        let num_qubits = diagram.num_qubits;
        let cols = assign_columns_svg(&diagram.instrs, num_qubits.max(1));
        let columns = group_by_column(&diagram.instrs, &cols);
        let num_cols = columns.len();

        let width = LEFT_MARGIN + (num_cols as f64) * COL_WIDTH + RIGHT_MARGIN;
        let clbit_block_height = if diagram.num_clbits > 0 {
            CLBIT_GAP + (diagram.num_clbits as f64) * ROW_HEIGHT
        } else {
            0.0
        };
        let height =
            TOP_MARGIN * 2.0 + (num_qubits.max(1) as f64) * ROW_HEIGHT + clbit_block_height;

        let mut body = String::new();

        // Wires + labels, drawn first so every gate glyph layers on top.
        for row in 0..num_qubits {
            let y = row_y(row);
            let _ = write!(
                body,
                r#"<text x="10" y="{:.1}" dominant-baseline="middle">{}</text>"#,
                y,
                esc(&diagram.wire_labels[row]),
            );
            let _ = write!(
                body,
                r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>"#,
                LEFT_MARGIN, y, width - RIGHT_MARGIN, y,
            );
        }

        // Classical wires: one double-line row per clbit, below every
        // qubit row, matching the conventional single-vs-double line
        // distinction between quantum and classical wires.
        for cb in 0..diagram.num_clbits {
            let y = row_y_clbit(cb, num_qubits);
            let _ = write!(
                body,
                r#"<text x="10" y="{:.1}" dominant-baseline="middle">c{}</text>"#,
                y, cb,
            );
            let offset = CLBIT_LINE_OFFSET;
            let _ = write!(
                body,
                r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>"#,
                LEFT_MARGIN, y - offset, width - RIGHT_MARGIN, y - offset,
            );
            let _ = write!(
                body,
                r#"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="black" stroke-width="1.5"/>"#,
                LEFT_MARGIN, y + offset, width - RIGHT_MARGIN, y + offset,
            );
        }

        for (c, instrs_here) in columns.iter().enumerate() {
            let x = col_x(c);
            for instr in instrs_here {
                match instr {
                    DiagramInstr::Single { qubit, label } => {
                        draw_box(&mut body, x, row_y(*qubit), label);
                    }
                    DiagramInstr::Measure { qubit, clbit } => {
                        if *clbit < diagram.num_clbits {
                            draw_box(&mut body, x, row_y(*qubit), "M");
                            let y_top = row_y(*qubit) + BOX_HALF_H;
                            let y_bottom = row_y_clbit(*clbit, num_qubits);
                            draw_double_vline(&mut body, x, y_top, y_bottom);
                        } else {
                            // No classical row to connect to (a
                            // malformed circuit whose clbit index
                            // exceeds num_clbits) -- fall back to the
                            // old self-contained label so the intended
                            // clbit is still visible even without a
                            // wire to draw it to.
                            draw_box(&mut body, x, row_y(*qubit), &format!("M->c{}", clbit));
                        }
                    }
                    DiagramInstr::Controlled { controls, target, target_label } => {
                        let (lo, hi) = instr.wire_range();
                        if lo != hi {
                            draw_vline(&mut body, x, row_y(lo), row_y(hi));
                        }
                        for &c_row in controls {
                            draw_dot(&mut body, x, row_y(c_row));
                        }
                        match target_label {
                            // CNOT's target ("X") gets the standard ⊕
                            // symbol; any other labeled target (e.g.
                            // `Cp`'s angle) keeps the boxed style,
                            // since it's a distinct labeled gate, not
                            // a XOR target.
                            Some(l) if l == "X" => draw_target_symbol(&mut body, x, row_y(*target)),
                            Some(l) => draw_box(&mut body, x, row_y(*target), l),
                            None => draw_dot(&mut body, x, row_y(*target)),
                        }
                    }
                    DiagramInstr::Span { qubits: (a, b), label } => {
                        let lo = *a.min(b);
                        let hi = *a.max(b);
                        let _ = write!(
                            body,
                            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="white" stroke="black" stroke-width="1.5"/>"#,
                            x - BOX_HALF_W,
                            row_y(lo) - BOX_HALF_H,
                            BOX_HALF_W * 2.0,
                            row_y(hi) - row_y(lo) + BOX_HALF_H * 2.0,
                        );
                        let mid_y = (row_y(lo) + row_y(hi)) / 2.0;
                        let _ = write!(
                            body,
                            r#"<text x="{:.1}" y="{:.1}" text-anchor="middle" dominant-baseline="middle">{}</text>"#,
                            x, mid_y, esc(label),
                        );
                    }
                    DiagramInstr::Swap { a, b } => {
                        let (lo, hi) = instr.wire_range();
                        draw_vline(&mut body, x, row_y(lo), row_y(hi));
                        draw_swap_marker(&mut body, x, row_y(*a));
                        draw_swap_marker(&mut body, x, row_y(*b));
                    }
                }
            }
        }

        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {:.1} {:.1}" font-family="monospace" font-size="14">{}</svg>"#,
            width, height, body,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::lower;

    #[test]
    fn single_qubit_gates_render_as_boxes() {
        let mut c = Circuit::new(1);
        c.push(Gate::H(0)).push(Gate::X(0));
        let d = Diagram::from_circuit(&c);
        let ascii = d.to_ascii();
        assert!(ascii.contains("[H]"));
        assert!(ascii.contains("[X]"));
        assert_eq!(ascii.lines().count(), 1, "one qubit -> one wire row");
    }

    #[test]
    fn cx_draws_a_control_dot_and_a_target_symbol() {
        let mut c = Circuit::new(2);
        c.push(Gate::Cx(0, 1));
        let ascii = Diagram::from_circuit(&c).to_ascii();
        assert!(ascii.contains('*'), "control dot missing:\n{}", ascii);
        assert!(ascii.contains("(+)"), "target symbol missing:\n{}", ascii);
    }

    #[test]
    fn distant_two_qubit_gate_shows_a_passthrough_connector() {
        // Cx(0, 2): qubit 1 isn't an argument, but the connecting line
        // must still visibly pass through its row.
        let mut c = Circuit::new(3);
        c.push(Gate::Cx(0, 2));
        let ascii = Diagram::from_circuit(&c).to_ascii();
        let lines: Vec<&str> = ascii.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains('|'), "expected a passthrough connector on q1:\n{}", ascii);
    }

    #[test]
    fn disjoint_gates_pack_into_the_same_column() {
        // H(0) and H(1) act on disjoint qubits and should render in
        // the same column (same horizontal position), not one after
        // another -- otherwise every "layer" test below would be
        // meaningless.
        let mut c = Circuit::new(2);
        c.push(Gate::H(0)).push(Gate::H(1));
        let cols = assign_columns(&Diagram::from_circuit(&c).instrs, 2);
        assert_eq!(cols, vec![0, 0]);
    }

    #[test]
    fn overlapping_range_forces_a_new_column() {
        // Cx(0, 2) followed by H(1): qubit 1 is inside Cx(0,2)'s wire
        // range even though Cx doesn't act on it directly, so H(1)
        // must be pushed to a later column, not packed alongside it.
        let mut c = Circuit::new(3);
        c.push(Gate::Cx(0, 2)).push(Gate::H(1));
        let cols = assign_columns(&Diagram::from_circuit(&c).instrs, 3);
        assert_eq!(cols, vec![0, 1]);
    }

    #[test]
    fn swap_renders_two_x_markers_connected() {
        let mut c = Circuit::new(2);
        c.push(Gate::Swap(0, 1));
        let ascii = Diagram::from_circuit(&c).to_ascii();
        let lines: Vec<&str> = ascii.lines().collect();
        assert!(lines[0].contains('X'));
        assert!(lines[1].contains('X'));
    }

    #[test]
    fn rzz_renders_as_a_shared_spanning_box() {
        let mut nc = NativeCircuit::new(2);
        nc.push(NativeGate::Rzz(0, 1, 0.5));
        let ascii = Diagram::from_native(&nc).to_ascii();
        let lines: Vec<&str> = ascii.lines().collect();
        assert!(lines[0].contains("RZZ"));
        assert!(lines[1].contains("RZZ"));
    }

    #[test]
    fn measure_shows_its_target_clbit() {
        let mut c = Circuit::new(1);
        c.num_clbits = 1;
        c.push(Gate::Measure(0, 0));
        let ascii = Diagram::from_circuit(&c).to_ascii();
        assert!(ascii.contains("M->c0"), "got:\n{}", ascii);
    }

    #[test]
    fn backend_rot_label_matches_the_backend_axis() {
        use crate::backend::Backend;
        let mut c = Circuit::new(1);
        c.push(Gate::H(0));

        let ion = lower(&c, Backend::TrappedIon);
        let ion_ascii = Diagram::from_backend(&ion).to_ascii();
        assert!(ion_ascii.contains("RY"), "TrappedIon should render Rot as RY:\n{}", ion_ascii);

        let ibm = lower(&c, Backend::IbmQ);
        let ibm_ascii = Diagram::from_backend(&ibm).to_ascii();
        assert!(ibm_ascii.contains("RX"), "IbmQ should render Rot as RX:\n{}", ibm_ascii);
    }

    #[test]
    fn ascii_columns_line_up_across_every_row() {
        // Every row must be exactly the same length -- otherwise the
        // "columns" aren't actually aligned when printed.
        let mut c = Circuit::new(3);
        c.push(Gate::H(0))
            .push(Gate::Cx(0, 1))
            .push(Gate::Rz(2, 0.1234))
            .push(Gate::Cx(1, 2))
            .push(Gate::Measure(0, 0));
        c.num_clbits = 1;
        let ascii = Diagram::from_circuit(&c).to_ascii();
        let lens: Vec<usize> = ascii.lines().map(|l| l.chars().count()).collect();
        assert!(lens.windows(2).all(|w| w[0] == w[1]), "misaligned rows:\n{}", ascii);
    }

    #[test]
    fn svg_output_is_well_formed_enough_to_be_valid_xml_shaped() {
        let mut c = Circuit::new(2);
        c.push(Gate::H(0)).push(Gate::Cx(0, 1));
        let svg = Diagram::from_circuit(&c).to_svg();
        assert!(svg.starts_with("<svg"));
        assert!(svg.trim_end().ends_with("</svg>"));
        // Every opened tag among the ones we emit should be closed --
        // a cheap, real structural check without pulling in an XML
        // parser dependency just for this.
        for tag in ["rect", "circle", "line", "text"] {
            let opens = svg.matches(&format!("<{}", tag)).count();
            // rect/circle/line/text here are always emitted as
            // self-contained tags ending in `/>` (rect/circle/line) or
            // with an explicit `</text>` close, never as an open tag
            // left dangling.
            let self_closed = svg.matches(&format!("<{} ", tag)).count();
            assert_eq!(opens, self_closed, "unclosed <{}> tag in:\n{}", tag, svg);
        }
    }

    #[test]
    fn empty_circuit_renders_without_panicking() {
        let c = Circuit::new(0);
        let d = Diagram::from_circuit(&c);
        assert_eq!(d.to_ascii(), "");
        let svg = d.to_svg();
        assert!(svg.starts_with("<svg"));
    }

    #[test]
    fn full_pipeline_diagram_is_consistent_at_every_level() {
        // Not a fidelity check (that's decompositions.rs's job) -- just
        // that all three levels of the same circuit produce a diagram
        // with the right qubit/instruction counts and don't panic.
        let mut c = Circuit::new(2);
        c.push(Gate::H(0)).push(Gate::Cx(0, 1)).push(Gate::Rz(1, 0.3));

        let source = Diagram::from_circuit(&c);
        assert_eq!(source.num_qubits, 2);
        assert_eq!(source.instrs.len(), 3);

        let native = Diagram::from_native(&crate::native::decompose(&c));
        assert_eq!(native.num_qubits, 2);
        assert!(!native.instrs.is_empty());

        let backend_circuit = lower(&c, crate::backend::Backend::IbmQ);
        let backend_diagram = Diagram::from_backend(&backend_circuit);
        assert_eq!(backend_diagram.num_qubits, backend_circuit.num_qubits);
        assert!(!backend_diagram.to_ascii().is_empty());
        assert!(!backend_diagram.to_svg().is_empty());
    }
}