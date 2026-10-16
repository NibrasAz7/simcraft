//! simcraft-cli — run `.simcraft` models headless.
//!
//!   simcraft run model.simcraft            run, print every sink trace
//!   simcraft check model.simcraft          validate only (compile, no run)
//!   simcraft blocks                        list the block catalog

use simblocks::{Scope, ToWorkspace};
use simcore::Sim;
use simmodel::Document;

fn main() {
    std::process::exit(real_main());
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
            let model = match doc.to_model(&simblocks::Factory) {
                Ok(m) => m,
                Err(e) => return fail(&file, &e),
            };
            let steps = (doc.solver.tstop / doc.solver.dt).round() as u64;
            let mut sim = match Sim::new(model, doc.solver) {
                Ok(s) => s,
                Err(e) => return fail(&file, &e),
            };
            if cmd == "check" {
                println!("{file}: ok — {} block(s), {} state(s), {steps} steps", sim.model().len(), sim.state().len());
                return 0;
            }
            if let Err(e) = sim.run() {
                return fail(&file, &e);
            }
            println!("{file}: ran {steps} steps ({}, dt={})", doc.solver.solver.name(), doc.solver.dt);
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
    eprintln!("usage: simcraft <run|check|blocks> [model.simcraft]");
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
}
