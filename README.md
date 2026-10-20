# SimCraft

SimCraft is a clean-room, open-source, Rust-native block-diagram modeling and
simulation environment — the Simulink-class member of the ArtCraft family, and
MatCraft's closest sibling. It runs natively on Windows, macOS and Linux, and on
the web via WASM.

Status: **S1–S3 working, S4 started** — `simcore` (typed graph,
compile-time signal propagation, algebraic-loop detection, `euler`/`rk4`
fixed-step + adaptive `rk45` Dormand–Prince solver), `simblocks` (25
blocks: sources, sinks, math, logic, continuous, discrete, routing),
`simmodel` (`.simcraft` JSON), `simcraft_engine` (command session +
MatCraft bridge: `model.*`, `block.*`, `sim.run`/`sim.step`, to-workspace
publish), `simcraft-cli` (`run`/`check`/`blocks` + solver overrides) and
Studio (egui palette/canvas/inspector, run + trace chart). `cargo run -p
simcraft-cli -- run examples/pendulum.simcraft` simulates a linearized
pendulum. Not yet: multirate/zero-crossing rigor, subsystems, M-function
blocks, WASM.
MatCraft (`../matcraft`) is the reference implementation for every shared
convention below.

## Why it exists

There is no open-source Simulink-class tool. Octave covers the language side
(GPL); the modeling/simulation side has nothing a student or indie developer can
reach without a MathWorks license. SimCraft fills that gap as the ArtCraft
family's dynamic-systems modeler — same licensing, same automation-first design,
same "everything is a command" architecture, tightly interconnected with
MatCraft the way Simulink interlocks with MATLAB.

## What it is

- A **block-diagram editor**: palette, canvas, ports, wires, subsystems,
  labels, notes, parameter dialogs.
- A **simulation engine**: typed directed graphs, continuous + discrete states,
  fixed- and variable-step solvers, sample times, zero crossings, scopes.
- A **programmable model API**: models are inspectable, versionable text files;
  every UI action is a command reachable from CLI, the control channel, MCP —
  and from M-code running in MatCraft.

## Honest scope

Target: genuinely useful educational and indie simulation — pendulums, control
loops, filters, discrete controllers, mixed-signal demos. Explicitly not
first-release goals: full Simulink parity, Stateflow-style state machines,
masked library authoring, code generation, hardware-in-the-loop, thousands of
toolbox blocks. Those are later milestones, sized honestly in the roadmap.

## The MatCraft interconnection (the core of this product)

Simulink is useful because MATLAB can drive it; SimCraft is designed the same
way. One linked MatCraft session is the model's compute context.

### M-code drives models

Inside MatCraft (and inside SimCraft Studio's embedded console):

```matlab
out = sim('pendulum.simcraft')                    % run, get logged outputs
set_param('pendulum/Integrator', 'x0', 0.2)       % set a block parameter
g   = get_param('pendulum/Gain', 'K')             % read a parameter
add_block('simcraft/Sources/Sine', 'pendulum/w1') % edit the model
add_line('pendulum', 'w1/1', 'Gain/1')            % wire it
simcraft_stop                                     % halt a running sim
```

`sim` returns a struct: `out.t` (time vector), `out.y`/`out.scope1` (logged
signals), `out.x` (final states), `out.stoptime`, `out.stats` (steps, solver,
elapsed). Sinks can also write `Value`s straight into the MatCraft workspace.

### Blocks and workspace exchange

- **From Workspace** reads a variable (or `{t,y}` struct) per sample time.
- **To Workspace** writes logged signals as workspace variables.
- **MATLAB Function block** evaluates user M-code per step — a first-class
  block whose body runs in the linked session (the Fcn block of our world).
- **Model callbacks** (`InitFcn`, `StartFcn`, `StopFcn`, `PreLoadFcn`) are
  M-code evaluated in the model's linked session — parameters can be computed,
  not just typed: `set_param(m,'Gain','K')` where `K` came from `sweep.m`.

### Shared infrastructure, nothing duplicated

| Concern | Shared from |
| --- | --- |
| Matrices, linalg | `matcore` (`Matrix`, `CMatrix`, `SparseMat`) |
| Values, M-code | `matlang` (`Session`, `Value`) |
| Scopes, plots | `matplot` (a Scope block is a streaming `Figure`) |
| Command registry | `engine` conventions (`sim.run`, `model.set_param`, …) |
| Automation | `mcp` protocol + TCP control channel, same JSON-lines framing |
| File exchange | `.mat` v5 (load/save for logged data), `.simcraft` model files |
| UI chrome | `ui-egui` patterns: docked panes, ribbon, control server, themes |

SimCraft Studio ships with a MatCraft console pane — `who`, `plot(out.t,
out.y)`, script files driving parameter sweeps — no second language to learn.

## Architecture

Layered like the siblings; nothing below L6 touches egui/winit:

```text
simcore   (L1) graph, ports, solver, scheduler — pure Rust, no egui
simblocks (L2) the block library (built on simcore traits)
simmodel  (L2) .simcraft file format, load/save/validate
engine    (L5) command registry: sim.run, model.*, block.*, wire.*, scope.*
ui-egui   (L6) diagram canvas, palette, scope panes, console (matlang)
mcp       (L6) MCP server for agents
apps/simcraft       desktop Studio (eframe)
apps/simcraft-cli   headless run/export: simcraft-cli sim model.simcraft
```

### simcore — the kernel

- **Model**: directed graph of blocks; ports are typed (`double`, `complex`,
  integer widths, `bool`, bus/struct) with width (`scalar | vector(n) | matrix(m,n)`).
- **`Block` trait**: `outputs()`, `derivatives()`, `update()` (discrete),
  `get_x()/set_x()` (continuous state), sample time, feedthrough flag.
- **Execution**: topological sort each step (algebraic order within a step);
  algebraic loops detected and either error or iterate via Newton solve
  (flagged, like Simulink's algebraic-loop diagnostic).
- **States**: continuous states extracted to a flat `x` vector owned by the
  solver; discrete states per-block with their own sample times.
- **Solvers**: `euler`, `rk4` (fixed-step); `rk45` adaptive Dormand–Prince
  5(4) with `rtol`/`atol` error control, `min_step`/`max_step` bounds, and
  accepted/rejected step stats (`sim.run` reports both). `dt` is the
  fixed step for `euler`/`rk4` and the initial step for `rk45`; all of it
  lives in the `.simcraft` `solver` block and is overridable per
  `sim.run`. Later: `ode15s`-style BDF for stiff systems, discrete-only
  fast path.
- **Scheduling**: multirate — each discrete block declares `Ts`; the scheduler
  fires updates on sample hits; zero-crossing detection brackets events and
  locates them by bisection/secant.
- **Signals**: `Signal { dims, dtype, ts }` propagated at compile-time model
  init — mismatched widths or rates fail before `t=0`, not mid-run.

### .simcraft model format

UTF-8 JSON, diff-friendly, `"format": "simcraft"` + `version`. Blocks keyed by
hierarchical path (`top/subsys/integrator`), each with `type`, `params`,
`position`; wires as `{src: "blk/port", dst: "blk/port"}`; model settings
(solver, stop time, tolerances, callbacks); comments allowed via a `note`
field. Validated on load; `model.check` reports unreachable blocks, unconnected
ports, width/rate mismatches.

### The block library (staged)

Current (~25): step, ramp, sine, constant, clock · scope, to-workspace ·
gain, sum, product (`*`/`/` per port), abs, saturation, dead-zone, switch ·
relational (`== ~= < <= > >=`), logical-operator (`and or xor nand nor
not`) · integrator, derivative, transfer-fcn, PID (parallel form with
derivative filter), transport-delay · unit-delay, zero-order-hold · mux,
demux.

Later: from-workspace, random, display, sign, math-fcn, state-space,
discrete-filter, relay, quantizer, lookup, manual-switch, subsystems +
enabled/triggered ports, bus creator, M-function block, second-order
integrator, rate transition, hit-crossing, S-function API (Rust `dyn
Block` + M-code blocks).

### Studio UI

Family-standard chrome (ribbon, docked panes, command console): left =
library browser (block palette, searchable); centre = diagram canvas
(hit-tested blocks, orthogonal/elbow wires, marquee select, drag-drop,
grid-snap, pan/zoom) + scope panes; right = inspector (selected block's
params) + model settings; bottom = MatCraft console. Simulation controls in
the ribbon: play/pause/step/stop, stop-time field, solver picker, sim speed.
Scopes render via matplot and can pop out into figure tabs — same document
system as MatCraft Studio.

### Automation

Everything is a command. `simcraft --control 7979` serves the same JSON-lines
protocol as MatCraft (`engine.execute`, `ui.screenshot`, `ui.render`,
`model.*`). `simcraft-cli` runs headless (`sim`, `check`, `export`),
`simcraft mcp` exposes the model to agents, and MatCraft's `eval.line` can
call `sim(...)` — a sweep script in MatCraft driving SimCraft is the
canonical demo.

## License

MIT OR Apache-2.0, like every ArtCraft app. Clean-room: public Simulink
documentation and published solver literature only; no MathWorks code, assets,
icons, file formats, or observed-behaviour capture from MathWorks software.
Block *names* and documented semantics are public knowledge; every
implementation here is our own.
