# SimCraft — instructions for agents

SimCraft is a clean-room, open-source, Rust-native block-diagram simulation
environment — the Simulink-class member of the ArtCraft family, interconnected
with MatCraft (`../matcraft`). Spec: [`README.md`](README.md); plan:
[`ROADMAP.md`](ROADMAP.md). It will run natively on macOS, Windows and Linux,
and on the web via WASM.

Status: **planning** — no crates exist yet. This file is written ahead of the
scaffold so the first commits start on the right conventions.

## Start every session here

1. Read [`ROADMAP.md`](ROADMAP.md) (milestone, what's next).
2. Read [`README.md`](README.md) §The MatCraft interconnection — the
   interconnection *is* the product.
3. Read `../matcraft/AGENTS.md` and `../matcraft/docs/architecture.md` —
   MatCraft is the reference implementation for every shared convention.
4. Behaviour reference: public Simulink documentation and published solver
   literature only (see Clean-room below).

## Non-negotiables

- **Clean-room: never use MathWorks software.** Simulink's license forbids
  reverse engineering. So:
  - Never launch, run, script, screenshot or measure Simulink or MATLAB (or
    copy Octave/Simulink-connector code), and never compare output against a
    MathWorks installation.
  - Never read or copy anything inside a MathWorks install (binaries, block
    libraries, help files, icons, sample models).
  - Behaviour comes only from public documentation and peer-reviewed /
    textbook solver literature (Dormand–Prince, Hairer & Wanner for BDF),
    cited by URL, and from models users send us that they own. Block *names*
    and documented semantics are fine; implementations are our own.
  - Never copy GPL/LGPL code (Octave is GPL — **do not read it**; Scilab/Xcos
    are GPL — **do not read them**), or any code whose licence is incompatible
    with MIT OR Apache-2.0.
- **Assets:** every non-code file is original, OSI open source, public domain
  or CC0, and has a row in `ASSETS.md` when assets land.
- **Never crash: no panics in non-test code.** Model files, control messages
  and MCP requests are untrusted. Fallible work returns `Result`; cap
  input-derived allocations (state vectors, logged signals, step counts), and
  index with `.get()` on data-derived indices. Workspace lints
  (`clippy::unwrap_used` etc.) enforce this; tests may unwrap.
- **Rust only.** No handwritten JS/TS; never break a future wasm target.
- **Layering** (to be enforced by `cargo xtask layers`):
  `simcore` (L1: graph+solver, no deps beyond matcore) → `simblocks`/`simmodel`
  (L2) → `engine` (L5) → `ui-egui`/`mcp` (L6) → `apps/*` (L7). Nothing below
  L6 may depend on egui/winit. `simcore` may depend on `matcore` and
  `matlang` (values, linked-session trait) but never on ui crates.
- **Everything is a command:** user-visible behaviour becomes a command id
  (`sim.run`, `model.set_param`, `block.add`, `wire.connect`, …) reachable
  from CLI, control channel and MCP — same pattern as MatCraft and the other
  siblings.
- **Quality gates** before every commit: `cargo xtask ci` (fmt, clippy -D
  warnings including no-panic lints, tests, layers).

## Design commitments (don't relitigate)

- One linked MatCraft session is the model's compute context: callbacks,
  M-function blocks and From/To Workspace blocks all evaluate there.
- Model files are diff-friendly JSON (`.simcraft`, `"format": "simcraft"`).
- A Scope block is a matplot `Figure` — pop-out figure tabs reuse MatCraft's
  document system.
- Signal typing/dimensions propagate at model init; mismatches fail before
  `t=0`, never mid-run.
