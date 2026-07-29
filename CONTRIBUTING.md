# Contributing to sirraya-qutub-transpiler

First off — thank you for considering a contribution. This project is a
QASM 2.0 importer and multi-backend native-gate compiler for quantum
circuits, built and maintained by [Sirraya Labs](<!-- TODO: org link -->).
We welcome issues, discussion, docs fixes, and code from anyone, regardless
of experience level with quantum computing or Rust.

This document gets you from "cloned the repo" to "opened a PR" with as
little friction as possible. For the deep technical dive into *why* the
codebase looks the way it does, see [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Table of contents

- [Code of Conduct](#code-of-conduct)
- [Quick start](#quick-start)
- [Ways to contribute](#ways-to-contribute)
- [Before you start](#before-you-start)
- [Development setup](#development-setup)
- [Project layout](#project-layout)
- [The one testing rule that matters most](#the-one-testing-rule-that-matters-most)
- [Coding conventions](#coding-conventions)
- [Commit messages](#commit-messages)
- [Pull request checklist](#pull-request-checklist)
- [Reporting bugs](#reporting-bugs)
- [Good first issues](#good-first-issues--where-help-is-wanted)
- [Getting help](#getting-help)
- [License](#license)

## Code of Conduct

This project follows the [Contributor Covenant](https://www.contributor-covenant.org/version/2/1/code_of_conduct/).
By participating, you're expected to uphold it. Report unacceptable
behavior to <!-- TODO: contact email -->.

## Quick start

```bash
git clone <!-- TODO: repo URL -->
cd sirraya-qutub-transpiler
cargo build
cargo test
```

If `cargo test` passes, you're set up correctly. If it doesn't, please
open an issue with your OS, Rust version (`rustc --version`), and the
full error — don't struggle through a broken setup silently, that's a bug
in our onboarding and we want to know about it.

**Prerequisites:**
- Rust, stable channel (`rustup default stable` if you're not sure which
  toolchain you're on).
- This crate depends on `sirraya_qutub` (the Qutub simulator, also from
  Sirraya Labs). `cargo build` will fetch it automatically per the
  `Cargo.toml` dependency declaration — no separate setup should be
  needed, but if you hit a resolution error, check that dependency's own
  README for anything version-specific.

## Ways to contribute

You don't need to write Rust to contribute meaningfully:

| Contribution | Where to start |
|---|---|
| Report a bug | [Open an issue](#reporting-bugs) |
| Fix a doc typo / unclear explanation | Just open a PR, no issue needed |
| Add test coverage | See [testing rule](#the-one-testing-rule-that-matters-most) below — always welcome |
| Pick up a labeled issue | [`good first issue`](#good-first-issues--where-help-is-wanted) |
| Propose a feature | Open a **discussion issue** first (see below) |
| Tackle an open roadmap item | See [`ARCHITECTURE.md` §5](ARCHITECTURE.md#5-current-status) for the current list of known gaps |

## Before you start

- **Search existing issues and PRs first** — someone may already be
  working on it, or it may already be a known, deliberate design decision
  (this codebase has a lot of "we thought about the obvious simplification
  and rejected it, here's why" — see `ARCHITECTURE.md` before assuming
  something's an oversight).
- **For anything beyond a small fix, open an issue before writing code.**
  This is especially true for anything touching gate decomposition,
  routing, or backend lowering — we'd rather agree on the approach with
  you up front than ask for a rewrite after the fact.
- Small, focused PRs get reviewed faster than large ones. If your change
  is naturally large (a new backend, a new coupling-map topology), it's
  fine — just say so in the issue first so review expectations are set.

## Development setup

```bash
cargo build              # build the crate
cargo test                # run the full test suite (fast — a few seconds)
cargo test <module>       # e.g. `cargo test route::` to scope to one module
cargo fmt                 # format
cargo fmt --check         # check formatting without changing anything (what CI runs)
cargo clippy --all-targets -- -D warnings   # lint (what CI runs)
```

There's no separate integration-test setup step — `tests/decompositions.rs`
and `tests/measurement.rs` run as part of `cargo test` and exercise the
real `sirraya_qutub` simulator directly, not mocks.

## Project layout

```
src/
  ir.rs            source-level gate set (Circuit, Gate)
  qasm.rs          OPENQASM 2.0 -> ir::Circuit parser
  ir_optimize.rs    source-level cancel/reorder pass
  route.rs         SWAP insertion against a CouplingMap
  coupling.rs      physical qubit connectivity (linear, heavy-hex, ...)
  native.rs        decomposition to {Rz, Ry, Rzz} (trapped-ion-style)
  backend.rs       decomposition to IbmQ / Rigetti / TrappedIon native gates
  optimize.rs      native-level peephole pass
  emit.rs          execution against sirraya_qutub + QASM re-emission
  fidelity.rs      gate-count-based fidelity estimate
tests/
  decompositions.rs   every gate identity, checked against the real simulator
  measurement.rs      shot-based statistical test for Gate::Measure
```

Full explanation of each module — what it does, the exact identities it
relies on, and the traps a naive rewrite would fall into — is in
[`ARCHITECTURE.md`](ARCHITECTURE.md).

## The one testing rule that matters most

**Every gate identity, decomposition, or optimization pass must be
checked against the real `sirraya_qutub::core::QuantumRegister`, not just
asserted algebraically.** The pattern (used throughout `tests/` and every
module's own `#[cfg(test)]` block):

1. Build a randomized initial state.
2. Run the "ground truth" side directly via `QuantumRegister`'s own
   `apply_*` methods.
3. Run your new code's side on a clone of the same initial state.
4. Compare with `QuantumRegister::fidelity` — expect `(fidelity - 1.0).abs()
   < 1e-9`.

A sign error anywhere shows up as fidelity ≪ 1, not a subtle discrepancy —
so this is a fast, reliable way to know your identity is *exactly* right,
not just plausible. `Gate::Measure` is the one exception (fidelity doesn't
apply to a collapsed state) — see `tests/measurement.rs`'s shot-based
approach instead.

**A PR adding a new identity without a test in this style will get asked
to add one before merge.** This isn't a formality — it's this project's
main quality guarantee, and it's what lets a single reviewer trust a
change quickly instead of re-deriving the math by hand.

## Coding conventions

- Run `cargo fmt` and `cargo clippy` clean before opening a PR (CI checks
  both).
- No `unsafe` unless there's genuinely no alternative — and if so, explain
  why in a comment right above it.
- This codebase leans heavily on doc comments that explain *why*, not
  just *what* — including rejected simpler alternatives. Please follow
  that style for anything non-obvious: a future contributor (possibly
  you, in six months) will thank you for not having to reverse-engineer
  the reasoning from git blame.
- Prefer exact identities over approximations. If something genuinely
  can't be exact (e.g. a fidelity estimate), say so explicitly in the doc
  comment, the way `fidelity.rs` does.

## Commit messages

Short, imperative, module-prefixed where it helps:

```
route: fix identity restoration on non-linear coupling maps
backend: add Rigetti-specific Rzz lowering
docs: clarify Measure's role in ir_optimize's commuting pass
```

Reference the issue number if there is one (`Fixes #42`). Squash-merge is
fine — commit history inside a PR branch doesn't need to be pristine, but
the PR title/description does, since that's what becomes the merge commit
message.

## Pull request checklist

Before you open (or mark ready-for-review):

- [ ] `cargo test` passes locally
- [ ] `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` pass
- [ ] New identity/decomposition/optimization ⇒ new fidelity- or
      shot-based test (see above) — not optional
- [ ] Touched `Gate::Measure` handling anywhere? Double-check
      `ir_optimize.rs`'s `disjoint` and `route.rs`'s remap logic — both
      special-case it on purpose
- [ ] Updated the relevant doc comment (and `ARCHITECTURE.md`, if the
      change is structural) so the docs don't go stale
- [ ] PR description explains *why*, not just *what* — link the issue if
      there is one

A maintainer will typically respond within <!-- TODO: response-time SLA,
e.g. "a few days" -->. If you haven't heard back in that window, a polite
ping on the PR is completely fine.

## Reporting bugs

Open an issue with:
- What you expected vs. what happened
- A minimal circuit (QASM text or `Circuit`-building code) that
  reproduces it
- Your Rust version and the crate commit/version
- Whether it reproduces on `main`

If you've found something that looks like it could produce a *wrong but
silently plausible* result (e.g. a fidelity that's off by a small amount
rather than obviously broken), please say so explicitly and treat it as
higher priority — those are the bugs this project's testing philosophy is
specifically designed to catch, so one slipping through is worth
understanding.

## Good first issues & where help is wanted

Check the repo's [`good first issue`](<!-- TODO: issue-tracker filter URL -->)
label for a current, maintained list. As a starting point, these are
known, well-scoped gaps (see `ARCHITECTURE.md` §5 for full context on
each):

- Give `Rigetti` its own real grid coupling map, instead of the
  conservative linear-chain stand-in it uses today.
- Add gate-specific commutation rules to `ir_optimize.rs`'s source-level
  pass (e.g. `Rz` commuting through a `Cx` control), matching the rules
  `backend.rs`'s native-level pass already uses.
- Extend test coverage for any identity that currently only has an
  algebraic derivation in a doc comment without a matching fidelity test.

Larger, more involved projects (open a discussion issue first):

- A `Backend::Pasqal` (neutral-atom) implementation — genuinely harder
  than the existing backends, since it needs atom placement and
  blockade-radius routing, not just fixed native-gate lowering.
- SWAP-count-aware routing in `route.rs` (currently a correctness pass,
  not an optimizer).

## Getting help

- **Questions about using the crate:** open a [Discussion](<!-- TODO: discussions URL -->)
  or an issue tagged `question`.
- **Stuck on a contribution:** comment on the issue or your draft PR —
  partial progress and "I'm not sure how to approach X" are welcome, not
  just finished work.
- **Something sensitive/security-related:** email <!-- TODO: security contact -->
  directly rather than filing a public issue.

## License

<!-- TODO: confirm license — e.g. "Licensed under MIT OR Apache-2.0, at
your option, matching Rust ecosystem convention. See LICENSE-MIT /
LICENSE-APACHE." By submitting a PR you agree your contribution is
licensed under the same terms. -->
