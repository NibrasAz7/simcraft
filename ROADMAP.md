# SimCraft roadmap

The Simulink-class ArtCraft app: block-diagram modeling + simulation,
interconnected with MatCraft. Specification: `README.md`.

## Milestones

Each milestone ends green: `cargo xtask ci` + a demo model that proves the stage.

### S1 — simcore kernel (headless)

- `Block` trait, typed ports/widths, topological scheduler
- Fixed-step `euler` + `rk4`; continuous state extraction to flat `x`
- `.simcraft` JSON load/save/validate (`simmodel`)
- Blocks (~20, M0 list in README): sources, sinks, math, continuous, routing, discrete
- `simcraft-cli sim model.simcraft --tstop 10` → logs to `.mat`/CSV
- Scope data rendered to PNG via matplot (headless figure export — already proven in MatCraft)
- **Done when**: `pendulum.simcraft` (integrators + gain + sum + sine + scope) simulates
  10 s at dt=1 ms headless; scope PNG shows a decaying oscillation; `model.check`
  reports unconnected ports and width mismatches.

### S2 — engine + studio skeleton

- `engine` command registry (`model.*`, `block.*`, `wire.*`, `sim.run/pause/stop`)
- Studio: library palette, canvas with drag/drop blocks + wires, inspector for params
- Run/pause/step/stop in the ribbon; stop-time and solver fields
- Control channel + MCP — `simcraft --control`, same protocol as MatCraft
- **Done when**: build a two-integrator system entirely in the GUI, run it, watch the
  scope draw live; `ui.screenshot` + `engine.execute` drive it remotely.

### S3 — MatCraft interconnection (the product thesis)

- Embedded MatCraft console pane in Studio (a `matlang::Session` pane)
- `sim`, `set_param`, `get_param`, `add_block`, `add_line` builtins in matlang
  (feature-gated `simcraft` dep — no circular layering: matlang gains a
  `SimModel` host-trait it calls *out* to; the app owns the link)
- From/To Workspace blocks; model callbacks (`InitFcn`/`StartFcn`/`StopFcn`) as M-code
- **Done when**: `out = sim('pendulum.simcraft'); plot(out.t, out.y)` works in the
  console; a `for`-loop sweep script in MatCraft runs 20 gains and plots the family.

### S4 — variable step + discrete rigor

- **`rk45` done**: Dormand–Prince 5(4) with rtol/atol error control,
  `min_step`/`max_step` bounds, accepted/rejected stats; trial steps land
  exactly on discrete sample hits and `tstop`. `sim.step` single-steps
  for debugging.
- Multirate discrete scheduler, sample-time propagation, inherited `-1` rates
- Zero-crossing detection + event location (step edges, relays)
- Algebraic-loop detection + optional Newton iteration
- **Done when**: rk45 with rtol=1e-4 on a stiff-ish demo takes <5% of euler's steps
  within tolerance; a relay-feedback demo reproduces its limit cycle; a mixed 1 ms/
  10 ms multirate model logs correctly-timed signals.

### S5 — modeling at scale

- Subsystems (atomic + enabled + triggered), model references
- Bus creator/selector, signal labels, goto/from tags
- M-function block, second-order integrator, transport delay, lookup tables, PID
- Library browser with user block libraries
- **Done when**: a cruise-control demo (plant + PID subsystem + enabled reset) simulates;
  a saved subsystem opens as its own model tab and wires into a parent.

### S6 — shipping

- WASM app (`simcraft-web`), macOS/Linux packaging, installer story
- S-function SDK (Rust `dyn Block` docs + M-code blocks), model codegen preview (Rust)
- Stiff solvers (BDF), `sim` with model reference hierarchies, performance pass
- Docs site + demo library (pendulum, bouncing ball w/ zero-crossing, cruise control,
  discrete PID, mixed-signal filter)
- **Done when**: `simcraft-web` runs a demo in-browser; installers ship; docs cover
  the block library and the M-code API.

## Explicitly later (multi-year, not promised)

Stateflow-class state machines, masked subsystem authoring UI, C codegen for
embedded targets, hardware-in-the-loop, fixed-point designer, Simscape-class
physical domains, the thousands-block library breadth.

## Conventions

Same as the family: MIT OR Apache-2.0 · clean-room (public docs only, no
MathWorks software — see `AGENTS.md`) · no panics in shipped code · everything
is a command · `cargo xtask ci` before every commit · layers enforced.
