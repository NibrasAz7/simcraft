//! simcraft-cli — run `.simcraft` models headless.
//!
//!   simcraft run model.simcraft [--solver rk45] [--dt 0.01] [--tstop 10]
//!                               [--rtol 1e-4] [--atol 1e-6]
//!                               [--min-step 0] [--max-step 0]
//!                                      run, print every sink trace
//!   simcraft check model.simcraft [same overrides]
//!                                      validate only (compile, no run)
//!   simcraft blocks                        list the block catalog

use simblocks::{Scope, ToWorkspace};
use simcore::{Sim, SimConfig, SimError, Solver};
use simmodel::Document;

fn main() {
    std::process::exit(real_main());
}

/// `--flag value` overrides onto the document's solver config.
fn apply_overrides(cfg: &mut SimConfig, args: &[String]) -> Result<(), SimError> {
    let bad = |m: String| SimError::Msg(m);
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let val = args.get(i + 1).ok_or_else(|| bad(format!("{flag} needs a value")))?;
        i += 2;
        let num = |dst: &mut f64| -> Result<(), SimError> {
            *dst = val.parse::<f64>().map_err(|_| bad(format!("{flag}: `{val}` is not a number")))?;
            Ok(())
        };
        match flag {
            "--solver" => {
                cfg.solver = Solver::parse(val).ok_or_else(|| bad(format!("--solver: unknown solver `{val}` (have: euler, rk4, rk45)")))?;
            }
            "--dt" => num(&mut cfg.dt)?,
            "--tstop" => num(&mut cfg.tstop)?,
            "--rtol" => num(&mut cfg.rtol)?,
            "--atol" => num(&mut cfg.atol)?,
            "--min-step" => num(&mut cfg.min_step)?,
            "--max-step" => num(&mut cfg.max_step)?,
            other => return Err(bad(format!("unknown flag `{other}`"))),
        }
    }
    Ok(())
}

fn real_main() -> i32 {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        usage();
        return 2;
    };
    match cmd.as_str() {
        "run" | "check" => {
            let Some(file) = args.next() else {
                usage();
                return 2;
            };
            let doc = match Document::load(std::path::Path::new(&file)) {
                Ok(d) => d,
                Err(e) => return fail(&file, &e),
            };
            let mut cfg = doc.solver;
            let rest: Vec<String> = args.collect();
            if let Err(e) = apply_overrides(&mut cfg, &rest) {
                return fail(&file, &e);
            }
            let model = match doc.to_model(&simblocks::Factory) {
                Ok(m) => m,
                Err(e) => return fail(&file, &e),
            };
            let mut sim = match Sim::new(model, cfg) {
                Ok(s) => s,
                Err(e) => return fail(&file, &e),
            };
            if cmd == "check" {
                println!("{file}: ok — {} block(s), {} state(s)", sim.model().len(), sim.state().len());
                return 0;
            }
            let out = match sim.run() {
                Ok(o) => o,
                Err(e) => return fail(&file, &e),
            };
            println!("{file}: ran {} steps ({}, dt={}, {} rejected)", out.steps, out.solver.name(), out.dt, out.rejected);
            let model = sim.model();
            for name in model.block_names() {
                if let Some(s) = model.block::<Scope>(name) {
                    print_trace(name, &s.trace);
                } else if let Some(tw) = model.block::<ToWorkspace>(name) {
                    print_trace(name, &tw.trace);
                }
            }
            0
        }
        "blocks" => {
            for b in simblocks::Factory::catalog() {
                println!("{:<16} {}", b.type_name, b.label);
            }
            0
        }
        _ => {
            usage();
            2
        }
    }
}

fn usage() {
    eprintln!(
        "usage: simcraft <run|check|blocks> [model.simcraft] [--solver s] [--dt h] [--tstop t] [--rtol r] [--atol a] [--min-step h] [--max-step h]"
    );
}

fn fail(file: &str, e: &simcore::SimError) -> i32 {
    eprintln!("{file}: error: {e}");
    1
}

/// Print a sink trace: first/last samples plus a summary — headless output,
/// so no giant dumps (full export comes with `simcraft run --csv`).
fn print_trace(name: &str, tr: &simblocks::Trace) {
    let n = tr.t.len();
    if n == 0 {
        println!("  {name}: empty trace");
        return;
    }
    let first = tr.t.first().copied().unwrap_or(0.0);
    let last = tr.t.last().copied().unwrap_or(0.0);
    let width = tr.y.first().map(|v| v.len()).unwrap_or(0);
    println!("  {name}: {n} samples over [{first}, {last}], width {width}");
    for (t, v) in tr.t.iter().zip(&tr.y).step_by((n / 10).max(1)).take(10) {
        println!("    t={t:>9.4}  {:?}", v);
    }
    if let (Some(t), Some(v)) = (tr.t.last(), tr.y.last()) {
        println!("    t={t:>9.4}  {:?}  (last)", v);
    }
}
