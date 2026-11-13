//! SimCraft MCP server.
//!
//! JSON-RPC 2.0 over newline-delimited stdio, implementing the MCP
//! lifecycle plus `tools` and `resources` — the same protocol shape as
//! MatCraft's `matcraft_mcp`, so shared clients work against either app.
//! Tools map onto engine commands: `execute` reaches every command by id,
//! `sim_run`/`model_load` are conveniences, and `simcraft://model` exposes
//! the loaded model's blocks/wires as a resource.
//!
//! The server session comes with a linked MatCraft workspace
//! (`session.link`), so `sim_run` publishes `out`/`{t, y}` structs and
//! scope traces land as matplot figures (`figure.list`) out of the box.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use simcraft_engine::Session;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

const INSTRUCTIONS: &str = "SimCraft is a block-diagram simulation environment (Simulink-class), \
interconnected with MatCraft. Models are .simcraft JSON files. Fastest path: `model_load` with a \
.simcraft path, then `sim_run` (optional solver/dt/tstop/rtol/atol overrides) — sink traces come \
back in `out` and publish into the linked MatCraft session (`out.t`, `out.y`, scope figures). \
`execute` reaches every engine command by id (list_commands shows ids and params). Read \
simcraft://model for the loaded model's blocks and wires.";

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

/// Tool result content: one text block carrying the JSON payload.
fn tool_result(payload: Value) -> Value {
    json!({"content": [{"type": "text", "text": payload.to_string()}]})
}

/// A tool call that failed at the command level (still a successful
/// JSON-RPC reply — MCP convention is `isError` content).
fn tool_error(message: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": message.into()}], "isError": true})
}

/// The MCP server: owns a headless engine session with a linked MatCraft
/// workspace.
pub struct Server {
    session: Session,
    initialized: bool,
}

impl Server {
    pub fn new() -> Self {
        let mut session = Session::new();
        // A linked MatCraft session is the model's compute context: it
        // receives `out` + `{t, y}` structs and hosts the scope figures.
        let _ = session.execute("session.link", json!({}));
        Server { session, initialized: false }
    }

    /// Serve newline-delimited JSON-RPC until `input` closes.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if let Some(reply) = self.handle_line(&line) {
                output.write_all(reply.as_bytes())?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// One line in, optional reply out (notifications get none).
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(Value::Array(batch)) if batch.is_empty() => Some(error(Value::Null, INVALID_REQUEST, "empty batch")),
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(m)).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(msg) => self.handle(msg),
            Err(e) => Some(error(Value::Null, PARSE_ERROR, format!("parse error: {e}"))),
        };
        reply.map(|r| r.to_string())
    }

    fn handle(&mut self, msg: Value) -> Option<Value> {
        let Value::Object(o) = &msg else {
            return Some(error(Value::Null, INVALID_REQUEST, "message must be an object"));
        };
        let id = o.get("id").cloned();
        let Some(method) = o.get("method").and_then(Value::as_str) else {
            if o.contains_key("result") || o.contains_key("error") {
                return None;
            }
            return Some(error(id.unwrap_or(Value::Null), INVALID_REQUEST, "missing `method`"));
        };
        let params = o.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            if method == "notifications/initialized" {
                self.initialized = true;
            }
            return None;
        };
        Some(match self.request(method, &params) {
            Ok(r) => response(id, r),
            Err((code, m)) => error(id, code, m),
        })
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "ping" => Ok(json!({})),
            "initialize" => {
                let client_v = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
                let version = if SUPPORTED.contains(&client_v) { client_v } else { PROTOCOL_VERSION };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": {"name": "simcraft", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "tools/list" => Ok(json!({"tools": tool_definitions()})),
            "tools/call" => self.call_tool(params),
            "resources/list" => Ok(json!({"resources": [
                {"uri": "simcraft://model", "name": "Loaded model",
                 "description": "The loaded .simcraft model's blocks and wires",
                 "mimeType": "application/json"},
                {"uri": "simcraft://commands", "name": "Command catalogue",
                 "description": "Every engine command id with params documentation",
                 "mimeType": "application/json"},
            ]})),
            "resources/read" => self.read_resource(params),
            _ => Err((METHOD_NOT_FOUND, format!("unknown method `{method}`"))),
        }
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "tools/call needs `name`".to_string()))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match name {
            "execute" => {
                let command = args.get("command").and_then(Value::as_str).ok_or((INVALID_PARAMS, "execute needs `command`".to_string()))?;
                let cmd_params = args.get("params").cloned().unwrap_or(json!({}));
                match self.session.execute(command, cmd_params) {
                    Ok(r) => Ok(tool_result(r)),
                    Err(e) => Ok(tool_error(e.to_string())),
                }
            }
            // The convenience tools forward their arguments as the command
            // params — sim_run's schema already mirrors sim.run's.
            "sim_run" => match self.session.execute("sim.run", args) {
                Ok(r) => Ok(tool_result(r)),
                Err(e) => Ok(tool_error(e.to_string())),
            },
            "model_load" => {
                let path = args.get("path").and_then(Value::as_str).ok_or((INVALID_PARAMS, "model_load needs `path`".to_string()))?;
                match self.session.execute("model.load", json!({"path": path})) {
                    Ok(r) => Ok(tool_result(r)),
                    Err(e) => Ok(tool_error(e.to_string())),
                }
            }
            "list_commands" => match self.session.execute("engine.list", json!({})) {
                Ok(r) => Ok(tool_result(r)),
                Err(e) => Ok(tool_error(e.to_string())),
            },
            _ => Ok(tool_error(format!("unknown tool `{name}`"))),
        }
    }

    fn read_resource(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let uri = params.get("uri").and_then(Value::as_str).ok_or((INVALID_PARAMS, "resources/read needs `uri`".to_string()))?;
        let result = match uri {
            // `{"loaded": false}` rather than an RPC error — an empty
            // session is a normal state, not a failure.
            "simcraft://model" if !self.session.loaded() => Ok(json!({"loaded": false})),
            "simcraft://model" => self.session.execute("model.list", json!({})),
            "simcraft://commands" => self.session.execute("engine.list", json!({})),
            _ => return Err((INVALID_PARAMS, format!("unknown resource `{uri}`"))),
        };
        match result {
            Ok(r) => Ok(json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": r.to_string()}]})),
            Err(e) => Err((INTERNAL_ERROR, e.to_string())),
        }
    }
}

impl Default for Server {
    fn default() -> Self {
        Server::new()
    }
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "model_load",
            "description": "Load a .simcraft model file (JSON: blocks, wires, solver settings).",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string", "description": "Path to the .simcraft file"}},
                "required": ["path"],
            },
        },
        {
            "name": "sim_run",
            "description": "Run the loaded model over [0, tstop]; returns the time grid plus every scope/to_workspace trace (which also publish into the linked MatCraft session, scopes as figures).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "solver": {"type": "string", "description": "euler | rk4 | rk45 (default: the document's)"},
                    "dt": {"type": "number", "description": "Fixed step (rk4/euler) or initial step (rk45)"},
                    "tstop": {"type": "number"},
                    "rtol": {"type": "number", "description": "rk45 relative tolerance"},
                    "atol": {"type": "number", "description": "rk45 absolute tolerance"},
                    "min_step": {"type": "number", "description": "rk45 minimum step (0 = unbounded)"},
                    "max_step": {"type": "number", "description": "rk45 maximum step (0 = unbounded)"},
                },
            },
        },
        {
            "name": "execute",
            "description": "Run any engine command by id with JSON params (list_commands shows the catalogue).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Command id, e.g. \"block.set\""},
                    "params": {"type": "object", "description": "Command parameters"},
                },
                "required": ["command"],
            },
        },
        {
            "name": "list_commands",
            "description": "List every engine command id with its label and params documentation.",
            "inputSchema": {"type": "object", "properties": {}},
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(server: &mut Server, msg: &str) -> Value {
        let out = server.handle_line(msg).unwrap_or_default();
        serde_json::from_str(&out).unwrap_or(Value::Null)
    }

    /// A two-block model file, written to a temp path for `model_load`.
    fn temp_model() -> std::path::PathBuf {
        let path = std::env::temp_dir().join("simcraft_mcp_test.simcraft");
        let doc = r#"{
            "format": "simcraft", "version": 1,
            "solver": {"solver": "rk4", "dt": 0.1, "tstop": 1.0},
            "blocks": [
                {"path": "c", "type": "constant", "params": {"value": 2.0}},
                {"path": "s", "type": "scope"}
            ],
            "wires": [{"from": "c/1", "to": "s/1"}]
        }"#;
        std::fs::write(&path, doc).unwrap();
        path
    }

    #[test]
    fn lifecycle_and_execute() {
        let mut s = Server::new();
        let init = line(&mut s, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}"#);
        assert_eq!(init["result"]["serverInfo"]["name"], "simcraft");
        // Notifications get no reply.
        assert!(s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        let r =
            line(&mut s, r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"execute","arguments":{"command":"engine.version"}}}"#);
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("simcraft"), "{text}");
    }

    #[test]
    fn tools_list_shows_the_tools() {
        let mut s = Server::new();
        let r = line(&mut s, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        let names: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        for want in ["execute", "sim_run", "model_load", "list_commands"] {
            assert!(names.contains(&want), "missing {want}");
        }
    }

    #[test]
    fn model_load_sim_run_and_model_resource() {
        let path = temp_model();
        let mut s = Server::new();
        // The model resource reports empty before a load.
        let r = line(&mut s, r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"simcraft://model"}}"#);
        let text = r["result"]["contents"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("\"loaded\":false"), "{text}");

        let r = line(
            &mut s,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"model_load","arguments":{{"path":"{}"}}}}}}"#,
                path.to_string_lossy().replace('\\', "\\\\")
            ),
        );
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("\"blocks\":2"), "{text}");

        let r = line(&mut s, r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sim_run","arguments":{}}}"#);
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("\"steps\""), "{text}");

        // The scope's trace became a figure in the linked session.
        let r = line(&mut s, r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"execute","arguments":{"command":"figure.list"}}}"#);
        let text = r["result"]["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("\"name\":\"s\""), "{text}");

        // And the resource now exposes the model's blocks and wires.
        let r = line(&mut s, r#"{"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"simcraft://model"}}"#);
        let text = r["result"]["contents"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("\"path\":\"c\""), "{text}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bad_requests() {
        let mut s = Server::new();
        let r = line(&mut s, "not json");
        assert_eq!(r["error"]["code"], -32700);
        let r = line(&mut s, r#"{"jsonrpc":"2.0","id":9,"method":"bogus/method"}"#);
        assert_eq!(r["error"]["code"], -32601);
        let r = line(&mut s, r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"model_load","arguments":{}}}"#);
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn tool_error_is_content_not_rpc_error() {
        let mut s = Server::new();
        // sim_run without a loaded model is a command-level error.
        let r = line(&mut s, r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sim_run","arguments":{}}}"#);
        assert_eq!(r["result"]["isError"], true);
        // Unknown commands through `execute` are too.
        let r = line(&mut s, r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"execute","arguments":{"command":"nope.cmd"}}}"#);
        assert_eq!(r["result"]["isError"], true);
    }
}
