# QuTub Guardrails

This implements the sequencing from `Roadmap.txt` / the equivalence-first
guidance already agreed on: **equivalence checking → fuzzing → provenance
→ (later) MLflow**, wired into CI as it grows past a single-maintainer
project.

## The core problem this solves

New contributors will touch `route.rs`, `ir_optimize.rs`, `native.rs` —
exactly the modules where a "seemingly innocent transformation" (the
roadmap doc's own phrase) breaks semantics. The guardrails need to catch
that *without* turning a first-time contributor's small PR into a wall
of unfamiliar red CI. The mechanism for that is **advisory-before-required**,
applied consistently:

| Check | File | Status | Blocks merge? |
|---|---|---|---|
| fmt / clippy / build / test | `guardrails-equivalence.yml` | **required from day 1** | yes — already true today, just not enforced in CI |

> **Note:** the original `.github/workflows/rust.yml` (build + `cargo test` only) has been renamed to
> `rust.yml.superseded-by-guardrails-equivalence.bak` rather than deleted — `guardrails-equivalence.yml`
> is a strict superset of what it did (same build/test, plus fmt, clippy, and the equivalence oracle),
> so keeping both active would just run build+test twice on every push. Nothing was removed from git
> history; the `.bak` file is inert (not a `.yml`, so Actions won't pick it up) and safe to delete once
> you've confirmed `guardrails-equivalence.yml` is green.

| Randomized equivalence oracle | `guardrails-equivalence.yml` | **required from day 1** | yes — `verify_equivalence.rs` already `exit(1)`s on fidelity < 1e-9, this just makes CI check it |
| PR fuzz smoke (90s) | `guardrails-fuzz-pr.yml` | advisory | no — `continue-on-error: true` |
| Nightly fuzz campaign (15 min/target) | `guardrails-fuzz-nightly.yml` | advisory | no — runs against `main` on a schedule, never against a PR |
| IR golden snapshots | `tests/ir_snapshots.rs` | required once added, but self-approving | a snapshot diff is expected and reviewed like any code diff, not a red X to argue with |
| Provenance manifest + metric drift | `guardrails-provenance.yml` | advisory | no — posts to the job summary only |

Two things make row 1 safe to make required *immediately*, unlike the rest:
it's zero new engineering (the oracle already exists and already fails
loudly), and it only runs on the exact code the contributor already
touched.

### Why fuzzing and provenance start advisory

- **Fuzzing** needs a burn-in period before its false-positive rate is
  known. A brand-new `cargo-fuzz` corpus finds real bugs *and* corner
  cases in the harness itself (e.g. a fuzzed circuit the harness
  shouldn't have generated in the first place). Blocking merges on that
  from day one punishes contributors for the harness's own bugs.
- **Provenance drift** (`--compare benchmarks/baseline/`) needs its
  10%-threshold tuned against real PR history before it's trustworthy —
  a routing-heavristic change *should* legitimately move gate counts.

### Promotion policy (advisory → required)

Promote one check at a time, and say so in the PR that flips it:

1. **Fuzz smoke** → required once the corpus has soaked ~2 weeks of
   nightly runs with no unreviewed crash, *and* at least one real bug
   has been caught and turned into a regression test (proof the check
   finds real things).
2. **Provenance drift** → required once `benchmarks/baseline/` has been
   refreshed from a trusted `main` run and the ±10% threshold has been
   checked against the last few months of routing/optimizer PRs without
   excessive false positives.
3. Formal equivalence (**MQT QCEC** as a second oracle, per Phase 3 of
   the roadmap doc) slots in as a third job in
   `guardrails-equivalence.yml` once wired — same required-from-day-1
   treatment as the simulator oracle, since it's a stronger version of
   the same claim.

### Keeping PRs fast

- `guardrails-fuzz-pr.yml` is **path-filtered** to the correctness-critical
  modules (`qasm.rs`, `ir_optimize.rs`, `optimize.rs`, `native.rs`,
  `route.rs`, `coupling.rs`, `backend.rs`). A docs PR or a new `examples/`
  file never triggers it.
- The 15-minute campaign only ever runs nightly against `main`, never
  against a PR branch.
- `guardrails-equivalence.yml` uses `cargo`/`target` caching keyed on
  `Cargo.lock`, so the required check stays fast after the first run.

### When a fuzz/nightly run finds something

`guardrails-fuzz-nightly.yml` auto-minimizes the crash
(`cargo fuzz tmin`) and opens (or comments on) a single tracking issue
labeled `fuzz-finding` — it never fails a PR someone is actively looking
at. The fix path is: minimize → add the minimized circuit under
`fuzz/regressions/<target>/` → add a `#[test]` in `tests/` that replays
it forever → if it's a semantic bug (not just a panic), also add it to
`verify_equivalence.rs`'s fixed corpus so the fidelity oracle has it too.

## What each new file is

- `.github/workflows/guardrails-equivalence.yml` — required PR gate:
  fmt, clippy, build, `cargo test`, and `verify_equivalence.rs`.
- `.github/workflows/guardrails-fuzz-pr.yml` — advisory 90s smoke fuzz,
  path-filtered.
- `.github/workflows/guardrails-fuzz-nightly.yml` — advisory 15-minute
  nightly campaign against `main`, corpus persisted via `actions/cache`,
  crashes filed as a tracking issue.
- `fuzz/` — standard `cargo-fuzz` layout, two targets:
  - `qasm_parse` — raw-bytes fuzzing of the QASM frontend for panics.
  - `ir_pipeline_no_panic` — structured circuit fuzzing (via `arbitrary`)
    through `optimize_ir → decompose → optimize → lower` for every
    backend, checking for panics/hangs. Complements, does not replace,
    `verify_equivalence.rs`'s fixed-corpus *fidelity* check.
- `tests/ir_snapshots.rs` — `insta` golden-file tests of the IR at each
  pipeline stage for Bell/GHZ/QFT, so a pass behavior change is a
  visible reviewable diff even when fidelity still checks out.
- `examples/provenance_manifest.rs` — implements the roadmap doc's own
  manifest schema (commit, passes, gate counts, depth, SWAP-relevant
  metrics, sha256 content hashes) per benchmark circuit, ahead of any
  MLflow dependency, exactly as the doc recommends.
- `.github/workflows/guardrails-provenance.yml` — advisory job that runs
  the manifest generator on every merge to `main` and diffs against
  `benchmarks/baseline/`.

## Dependencies to add

`Cargo.toml` `[dev-dependencies]`:
```toml
insta = { version = "1", features = ["yaml"] }
sha2 = "0.10"
serde_json = "1"
```
`fuzz/Cargo.toml` is already self-contained (its own crate, per
`cargo-fuzz` convention) and does not affect the main crate's dependency
tree or MSRV.

## Not included yet, deliberately

- **MQT QCEC formal equivalence** — Phase 3 in the roadmap doc, for
  good reason: it's a Python dependency in a Rust project and needs its
  own subprocess/FFI decision first. Slot it into
  `guardrails-equivalence.yml` as a second oracle once that's decided.
- **Mutation testing (`cargo-mutants`)** — worth adding once the test
  suite is large enough that "does the suite actually catch bugs in
  `route.rs`/`native.rs`" becomes a real question rather than an
  obvious yes. Same advisory-first treatment would apply: a weekly job,
  never blocking.
- **Differential testing against `qiskit_transpile_compare.py`** — the
  repo already has the harness; wiring it into CI is mostly a Python
  install cost. Good candidate for a weekly advisory job once someone
  wants to spend the CI minutes on it.
