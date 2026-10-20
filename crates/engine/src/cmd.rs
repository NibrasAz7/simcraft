//! The command registry. Each command: stable id, label, params doc, and a
//! `run` over the session. Same JSON-RPC-style contract as MatCraft's
//! engine — the CLI, control channel and MCP all reach these.

use serde_json::{json, Value as Json};

use simcore::{Param, Params};

use crate::{catalog_json, check_json, run_json, step_json, EngineError, RunOpts, Session};

/// A registered command.
pub struct CommandSpec {
    /// Stable id, `area.verb` style: `model.load`, `sim.run`.
    pub id: &'static str,
    /// One-line description.
    pub label: &'static str,
    /// JSON params documentation (for `engine.list` and MCP tool schemas).
    pub params: &'static str,
    pub run: fn(&mut Session, &Json) -> Result<Json, EngineError>,
}

/// Every command, in stable order.
pub fn command_specs() -> &'static [CommandSpec] {
    COMMANDS
}

pub fn find_command(id: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.id == id)
}

fn need_str<'a>(p: &'a Json, key: &str, cmd: &str) -> Result<&'a str, EngineError> {
    p.get(key).and_then(Json::as_str).ok_or_else(|| EngineError::bad_params(cmd, format!("missing string `{key}`")))
}

/// `sim.run`/`sim.step`'s `{solver?, dt?, tstop?, rtol?, atol?,
/// min_step?, max_step?}` overrides.
fn run_opts(p: &Json, cmd: &str) -> Result<RunOpts, EngineError> {
    let solver = match p.get("solver") {
        None | Some(Json::Null) => None,
        Some(v) => {
            let name = v.as_str().ok_or_else(|| EngineError::bad_params(cmd, "`solver` must be a string"))?;
            Some(
                simcore::Solver::parse(name)
                    .ok_or_else(|| EngineError::bad_params(cmd, format!("unknown solver `{name}` (have: euler, rk4, rk45)")))?,
            )
        }
    };
    let num = |key: &str| -> Result<Option<f64>, EngineError> {
        match p.get(key) {
            None | Some(Json::Null) => Ok(None),
            Some(v) => v.as_f64().map(Some).ok_or_else(|| EngineError::bad_params(cmd, format!("`{key}` must be a number"))),
        }
    };
    Ok(RunOpts {
        solver,
        dt: num("dt")?,
        tstop: num("tstop")?,
        rtol: num("rtol")?,
        atol: num("atol")?,
        min_step: num("min_step")?,
        max_step: num("max_step")?,
    })
}

/// `Param` → JSON (`number | [numbers] | string | bool`).
fn param_json(p: &Param) -> Json {
    match p {
        Param::Num(n) => json!(n),
        Param::Vec(v) => json!(v),
        Param::Str(s) => json!(s),
        Param::Bool(b) => json!(b),
    }
}

fn params_json(ps: &Params) -> Json {
    Json::Object(ps.iter().map(|(k, v)| (k.clone(), param_json(v))).collect())
}

/// JSON → `Param` for `block.set`; input-derived vectors are capped.
fn json_to_param(v: &Json) -> Result<Param, String> {
    match v {
        Json::Number(n) => n.as_f64().map(Param::Num).ok_or_else(|| "param number out of range".to_string()),
        Json::String(s) => Ok(Param::Str(s.clone())),
        Json::Bool(b) => Ok(Param::Bool(*b)),
        Json::Array(a) => {
            if a.len() > simcore::limits::MAX_SIGNAL_WIDTH {
                return Err(format!("param array too large (max {})", simcore::limits::MAX_SIGNAL_WIDTH));
            }
            a.iter().map(|x| x.as_f64().ok_or_else(|| "param arrays must be numbers".to_string())).collect::<Result<Vec<f64>, _>>().map(Param::Vec)
        }
        _ => Err("params must be numbers, arrays, strings or bools".to_string()),
    }
}

fn engine_version(_s: &mut Session, _p: &Json) -> Result<Json, EngineError> {
    Ok(json!({"app": "simcraft", "version": env!("CARGO_PKG_VERSION")}))
}

fn engine_list(_s: &mut Session, _p: &Json) -> Result<Json, EngineError> {
    let cmds: Vec<Json> = COMMANDS.iter().map(|c| json!({"id": c.id, "label": c.label, "params": c.params})).collect();
    Ok(json!({"commands": cmds}))
}

fn model_load(s: &mut Session, p: &Json) -> Result<Json, EngineError> {
    let path = need_str(p, "path", "model.load")?;
    let blocks = s.load(path)?;
    let wires = s.doc().map(|d| d.wires().len()).unwrap_or(0);
    Ok(json!({"ok": true, "path": path, "blocks": blocks, "wires": wires}))
}

fn model_check(s: &mut Session, _p: &Json) -> Result<Json, EngineError> {
    Ok(check_json(&s.check()?))
}

fn model_list(s: &mut Session, _p: &Json) -> Result<Json, EngineError> {
    let doc = s.doc().ok_or_else(|| EngineError::Sim(simcore::SimError::Msg("no model loaded — `model.load` a `.simcraft` file first".into())))?;
    let blocks: Vec<Json> = doc.blocks().iter().map(|(path, ty, ps)| json!({"path": path, "type": ty, "params": params_json(ps)})).collect();
    let wires: Vec<Json> = doc.wires().iter().map(|(from, to)| json!({"from": from, "to": to})).collect();
    Ok(json!({"blocks": blocks, "wires": wires}))
}

fn block_list(_s: &mut Session, _p: &Json) -> Result<Json, EngineError> {
    Ok(catalog_json())
}

fn block_get(s: &mut Session, p: &Json) -> Result<Json, EngineError> {
    let path = need_str(p, "path", "block.get")?;
    match s.doc().and_then(|d| d.block(path)) {
        Some((ty, ps)) => Ok(json!({"path": path, "type": ty, "params": params_json(ps)})),
        None => Err(EngineError::Sim(simcore::SimError::Msg(format!("no block `{path}`")))),
    }
}

fn block_set(s: &mut Session, p: &Json) -> Result<Json, EngineError> {
    let path = need_str(p, "path", "block.set")?;
    let obj = p.get("params").and_then(Json::as_object).ok_or_else(|| EngineError::bad_params("block.set", "missing `params` object"))?;
    let mut params = Params::new();
    for (k, v) in obj {
        params.insert(k.clone(), json_to_param(v).map_err(|m| EngineError::bad_params("block.set", m))?);
    }
    s.set_block_params(path, &params)?;
    Ok(json!({"ok": true, "path": path}))
}

fn sim_run(s: &mut Session, p: &Json) -> Result<Json, EngineError> {
    let ov = run_opts(p, "sim.run")?;
    Ok(run_json(&s.run(&ov)?))
}

fn sim_step(s: &mut Session, p: &Json) -> Result<Json, EngineError> {
    let ov = run_opts(p, "sim.step")?;
    let n = match p.get("n") {
        None | Some(Json::Null) => 1,
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| EngineError::bad_params("sim.step", "`n` must be a positive integer"))?;
            n.clamp(1, 1_000_000)
        }
    };
    Ok(step_json(&s.step(&ov, n)?))
}

fn session_link(s: &mut Session, _p: &Json) -> Result<Json, EngineError> {
    s.link(matlang::Session::new());
    Ok(json!({"ok": true, "linked": true}))
}

fn session_unlink(s: &mut Session, _p: &Json) -> Result<Json, EngineError> {
    s.unlink();
    Ok(json!({"ok": true, "linked": false}))
}

fn session_status(s: &mut Session, _p: &Json) -> Result<Json, EngineError> {
    Ok(json!({"loaded": s.loaded(), "linked": s.mat().is_some()}))
}

static COMMANDS: &[CommandSpec] = &[
    CommandSpec { id: "engine.version", label: "App name and version", params: "{}", run: engine_version },
    CommandSpec { id: "engine.list", label: "List every command", params: "{}", run: engine_list },
    CommandSpec { id: "model.load", label: "Load a .simcraft model file", params: "{path} → {blocks,wires}", run: model_load },
    CommandSpec { id: "model.check", label: "Compile-check the loaded model", params: "{} → {blocks,wires,states,steps}", run: model_check },
    CommandSpec { id: "model.list", label: "List the loaded model's blocks and wires", params: "{} → {blocks,wires}", run: model_list },
    CommandSpec { id: "block.list", label: "List the block catalog", params: "{} → {blocks}", run: block_list },
    CommandSpec { id: "block.get", label: "Read one block's type and params", params: "{path} → {path,type,params}", run: block_get },
    CommandSpec { id: "block.set", label: "Merge params into a block (rebuilds the model)", params: "{path, params:{...}}", run: block_set },
    CommandSpec {
        id: "sim.run",
        label: "Run the loaded model, collect sink traces",
        params: "{solver?, dt?, tstop?, rtol?, atol?, min_step?, max_step?} → {t, out:{name:{t,y}}}",
        run: sim_run,
    },
    CommandSpec {
        id: "sim.step",
        label: "Single-step the loaded model (debugging); solver opts apply at creation",
        params: "{n?, solver?, dt?, tstop?, rtol?, atol?, min_step?, max_step?} → {t, steps_done, finished, x}",
        run: sim_step,
    },
    CommandSpec { id: "session.link", label: "Attach a MatCraft workspace for to_workspace traces", params: "{}", run: session_link },
    CommandSpec { id: "session.unlink", label: "Detach the MatCraft workspace", params: "{}", run: session_unlink },
    CommandSpec { id: "session.status", label: "Loaded-model and link state", params: "{}", run: session_status },
];

#[cfg(test)]
mod tests {
    use super::*;

    const PENDULUM: &str = r#"{
        "format": "simcraft", "version": 1,
        "solver": {"solver": "rk4", "dt": 0.001, "tstop": 10.0},
        "blocks": [
            {"path": "theta_dot", "type": "integrator", "params": {"x0": 0.0}},
            {"path": "theta", "type": "integrator", "params": {"x0": 0.2}},
            {"path": "gl", "type": "gain", "params": {"gain": -9.81}},
            {"path": "angle_scope", "type": "scope"},
            {"path": "save_theta", "type": "to_workspace", "params": {"var": "theta"}}
        ],
        "wires": [
            {"from": "theta/1", "to": "gl/1"},
            {"from": "gl/1", "to": "theta_dot/1"},
            {"from": "theta_dot/1", "to": "theta/1"},
            {"from": "theta/1", "to": "angle_scope/1"},
            {"from": "theta/1", "to": "save_theta/1"}
        ]
    }"#;

    fn loaded() -> Session {
        let mut s = Session::new();
        s.load_document(simmodel::Document::parse(PENDULUM).unwrap()).unwrap();
        s
    }

    #[test]
    fn load_check_list() {
        let mut s = loaded();
        let r = s.execute("model.check", json!({})).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["blocks"], json!(5));
        assert_eq!(r["states"], json!(2));
        assert_eq!(r["steps"], json!(10000));
        let l = s.execute("model.list", json!({})).unwrap();
        assert_eq!(l["blocks"].as_array().unwrap().len(), 5);
        assert_eq!(l["wires"].as_array().unwrap().len(), 5);
        let b = s.execute("block.get", json!({"path": "gl"})).unwrap();
        assert_eq!(b["type"], json!("gain"));
        assert_eq!(b["params"]["gain"], json!(-9.81));
    }

    #[test]
    fn run_collects_traces_and_publishes() {
        let mut s = loaded();
        s.execute("session.link", json!({})).unwrap();
        let r = s.execute("sim.run", json!({})).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["solver"], json!("rk4"));
        assert_eq!(r["t"].as_array().unwrap().len(), 10001);
        let theta = &r["out"]["theta"];
        assert_eq!(theta["t"].as_array().unwrap().len(), 10001);
        assert_eq!(theta["y"][0][0], json!(0.2));
        // angle_scope logged the same signal under its block path.
        assert_eq!(r["out"]["angle_scope"]["y"][0][0], json!(0.2));
        // The linked MatCraft workspace got `out` + the `theta` struct.
        let mat = s.mat().unwrap();
        let out = mat.get("out").unwrap();
        assert!(format!("{out:?}").contains("theta"));
        // ~1 period of cos(√9.81·t): theta(10s) ≈ 0.2 again.
        let theta_var = mat.get("theta").unwrap();
        assert!(matches!(theta_var, matlang::Value::Struct(_)));
    }

    #[test]
    fn rk45_run_collects_and_publishes_same_shape() {
        let mut s = loaded();
        s.execute("session.link", json!({})).unwrap();
        let r = s.execute("sim.run", json!({"solver": "rk45", "rtol": 1e-6, "atol": 1e-8})).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["solver"], json!("rk45"));
        let t = r["t"].as_array().unwrap();
        assert!(t.len() > 2 && t.len() < 10001, "adaptive grid: {} points", t.len());
        // Nonuniform grid: successive spacings differ.
        let d = |i: usize| t[i + 1].as_f64().unwrap() - t[i].as_f64().unwrap();
        assert!((d(0) - d(t.len() / 2)).abs() > 1e-9);
        // Trace names/first samples identical to the fixed-step path.
        assert_eq!(r["out"]["theta"]["y"][0][0], json!(0.2));
        assert_eq!(r["out"]["angle_scope"]["y"][0][0], json!(0.2));
        let mat = s.mat().unwrap();
        assert!(mat.get("out").is_some() && mat.get("theta").is_some());
    }

    #[test]
    fn sim_step_single_steps_then_run_resets() {
        let mut s = loaded();
        let r = s.execute("sim.step", json!({})).unwrap();
        assert!((r["t"].as_f64().unwrap() - 0.001).abs() < 1e-12);
        assert_eq!(r["finished"], json!(false));
        let r = s.execute("sim.step", json!({"n": 9})).unwrap();
        assert!((r["t"].as_f64().unwrap() - 0.01).abs() < 1e-9);
        // Step to the end (n capped internally).
        let r = s.execute("sim.step", json!({"n": 100000})).unwrap();
        assert_eq!(r["finished"], json!(true));
        // sim.run afterwards runs fresh from t=0 with full history.
        let r = s.execute("sim.run", json!({})).unwrap();
        assert_eq!(r["t"].as_array().unwrap().len(), 10001);
        // And stepping after a run starts a fresh stepping sim.
        let r = s.execute("sim.step", json!({"solver": "rk45"})).unwrap();
        assert!(r["t"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn rk45_lands_on_discrete_sample_hits() {
        // unit_delay with Ts=0.37 — deliberately not a multiple of dt.
        let doc = r#"{
            "format": "simcraft", "version": 1,
            "solver": {"solver": "rk45", "dt": 0.05, "tstop": 1.2, "rtol": 1e-6},
            "blocks": [
                {"path": "c", "type": "constant", "params": {"value": 3.0}},
                {"path": "d", "type": "unit_delay", "params": {"sample_time": 0.37}},
                {"path": "s", "type": "scope"}
            ],
            "wires": [{"from": "c/1", "to": "d/1"}, {"from": "d/1", "to": "s/1"}]
        }"#;
        let mut s = Session::new();
        s.load_document(simmodel::Document::parse(doc).unwrap()).unwrap();
        let r = s.execute("sim.run", json!({})).unwrap();
        let t: Vec<f64> = r["out"]["s"]["t"].as_array().unwrap().iter().filter_map(Json::as_f64).collect();
        // Every hit 0.37·k <= tstop appears exactly in the log.
        for hit in [0.37, 0.74, 1.11] {
            assert!(t.iter().any(|&x| (x - hit).abs() < 1e-12), "no log point at {hit}: {t:?}");
        }
        assert_eq!(t.last().copied(), Some(1.2));
        // Held value is the sampled constant throughout.
        assert!(r["out"]["s"]["y"].as_array().unwrap().iter().all(|row| row[0] == json!(3.0)));
    }

    #[test]
    fn block_set_changes_params() {
        let mut s = loaded();
        s.execute("block.set", json!({"path": "gl", "params": {"gain": -1.0}})).unwrap();
        let b = s.execute("block.get", json!({"path": "gl"})).unwrap();
        assert_eq!(b["params"]["gain"], json!(-1.0));
        let r = s.execute("sim.run", json!({"tstop": 0.1})).unwrap();
        // theta'' = -theta now: slower, and the first sample is still x0.
        assert_eq!(r["out"]["theta"]["y"][0][0], json!(0.2));
        assert_eq!(r["t"].as_array().unwrap().len(), 101);
    }

    #[test]
    fn errors_not_panics() {
        let mut s = Session::new();
        assert!(matches!(s.execute("nope.cmd", json!({})), Err(EngineError::UnknownCommand(_))));
        assert!(s.execute("model.check", json!({})).is_err());
        assert!(s.execute("sim.run", json!({})).is_err());
        assert!(s.execute("model.load", json!({"path": "does/not/exist.simcraft"})).is_err());
        // Solver and dt validation happens at Sim::new.
        assert!(s.execute("sim.run", json!({"solver": "ode45"})).is_err());
    }

    #[test]
    fn engine_lists_commands() {
        let mut s = Session::new();
        let r = s.execute("engine.list", json!({})).unwrap();
        let ids: Vec<&str> = r["commands"].as_array().unwrap().iter().filter_map(|c| c["id"].as_str()).collect();
        for want in ["model.load", "model.check", "model.list", "block.list", "block.get", "block.set", "sim.run"] {
            assert!(ids.contains(&want), "missing {want}");
        }
    }
}
