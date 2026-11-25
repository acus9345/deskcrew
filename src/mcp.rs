// `deskcrew mcp`: the same commands as MCP tools over stdio, for clients that call tools
// instead of running a shell (Claude Desktop, Codex, Cursor and the like).
//
// Each call runs this program again with the command's words, so a tool does exactly what the
// command line does, and the service keeps each agent's cursor and windows between calls.
use crate::service;
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    process::{Command, Stdio},
};

const LATEST_PROTOCOL: &str = "2025-06-18";

fn tools() -> Value {
    json!([
        {
            "name": "see",
            "description": "Read a window's controls as refs (e3, e4...) with names and values. Later reads show only what changed. Read once, then do every step with `do`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "window": { "type": "string", "description": "Title, part of it, app name or w-ref from `windows`" },
                    "full": { "type": "boolean", "description": "Everything again, not only changes" },
                    "all": { "type": "boolean", "description": "Off-screen controls too" },
                    "find": { "type": "string", "description": "Only controls matching this word" }
                },
                "required": ["window"]
            }
        },
        {
            "name": "do",
            "description": "Run many steps in one call, separated by `;`, e.g. \"set Name Ada; select Country Japan; check 'Gift wrap'; click Submit; click Yes\". Names work in place of refs, and it goes on into dialogs the steps open. Replies with everything that changed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "steps": { "type": "string" },
                    "window": { "type": "string" },
                    "dry": { "type": "boolean", "description": "Check the steps without running them" }
                },
                "required": ["steps", "window"]
            }
        },
        {
            "name": "shot",
            "description": "A picture of a window, the screen or a region, without bringing anything forward. Use it only where `see` finds no controls.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "window": { "type": "string", "description": "Window, or a ref like e3 for one control" },
                    "screen": { "type": "boolean", "description": "The whole screen instead of a window" },
                    "region": { "type": "string", "description": "x,y,w,h of the window or screen" },
                    "marks": { "type": "boolean", "description": "Label controls with their refs" },
                    "rulers": { "type": "boolean", "description": "Pixel rulers for x,y clicks" }
                }
            }
        },
        {
            "name": "deskcrew",
            "description": format!("Any other deskcrew command, as its words: [\"click\", \"e3\", \"-w\", \"Calculator\"].\n{}", service::HELP),
            "inputSchema": {
                "type": "object",
                "properties": { "args": { "type": "array", "items": { "type": "string" } } },
                "required": ["args"]
            }
        }
    ])
}

/// The name an MCP client gives itself, as the cursor label people know it by.
fn agent_name(client: &str) -> String {
    let lower = client.to_lowercase();
    let known = [("claude-code", "Claude Code"), ("claude", "Claude"), ("codex", "Codex"), ("cursor", "Cursor"), ("gemini", "Gemini"), ("copilot", "Copilot"), ("visual studio code", "Copilot"), ("windsurf", "Windsurf"), ("cline", "Cline"), ("zed", "Zed"), ("antigravity", "Antigravity"), ("grok", "Grok"), ("kiro", "Kiro")];
    known.iter().find(|(part, _)| lower.contains(part)).map_or_else(|| client.to_string(), |(_, name)| name.to_string())
}

fn text(s: &str) -> Value {
    json!({ "type": "text", "text": s })
}

/// The command line a tool call stands for.
fn words(tool: &str, a: &Value) -> Result<Vec<String>, String> {
    let s = |key: &str| a.get(key).and_then(Value::as_str).map(str::to_string);
    let on = |key: &str| a.get(key).and_then(Value::as_bool).unwrap_or(false);
    let need = |key: &str| s(key).filter(|v| !v.is_empty()).ok_or(format!("`{key}` is required"));
    let reject_agent = |value: &str| {
        if value == "--agent" {
            Err("the agent name is set by the MCP client, not --agent".to_string())
        } else {
            Ok(value.to_string())
        }
    };
    let mut out: Vec<String> = Vec::new();
    match tool {
        "see" => {
            out.push("see".into());
            if on("full") {
                out.push("-f".into());
            }
            if on("all") {
                out.push("-a".into());
            }
            if let Some(word) = s("find") {
                out.extend(["--find".into(), reject_agent(&word)?]);
            }
            out.extend(["--".into(), need("window")?]);
        }
        "do" => {
            out.extend(["do".into(), "-w".into(), reject_agent(&need("window")?)?]);
            if on("dry") {
                out.push("--dry".into());
            }
            out.extend(["--".into(), need("steps")?]);
        }
        "shot" => {
            out.push("shot".into());
            if on("screen") {
                out.push("--screen".into());
            }
            if let Some(r) = s("region") {
                out.extend(["--region".into(), reject_agent(&r)?]);
            }
            if on("marks") {
                out.push("--marks".into());
            }
            if on("rulers") {
                out.push("--rulers".into());
            }
            if let Some(w) = s("window") {
                out.extend(["--".into(), w]);
            }
        }
        "deskcrew" => {
            let list = a.get("args").and_then(Value::as_array).ok_or("`args` is required: the command's words")?;
            out = list.iter().map(|w| w.as_str().map(str::to_string).unwrap_or_else(|| w.to_string())).collect();
            let flag_limit = out.iter().position(|w| w == "--").unwrap_or(out.len());
            if out[..flag_limit].iter().any(|w| w == "--agent") {
                return Err("the agent name is set by the MCP client, not --agent".into());
            }
            match out.first().map(String::as_str) {
                None => return Err("`args` is empty: [\"help\"] lists the commands".into()),
                Some("mcp" | "service") => return Err("that command does not run as a tool".into()),
                _ => {}
            }
        }
        _ => return Err(format!("no tool named {tool}")),
    }
    Ok(out)
}

/// Run one command as this same program, the way an agent's shell would.
fn run(agent: &str, args: &[String]) -> (bool, String) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return (false, e.to_string()),
    };
    let mut command = Command::new(exe);
    command.env("DESKCREW_AGENT", agent).args(args).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    match command.output() {
        Ok(out) => {
            let mut reply = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
            let errors = String::from_utf8_lossy(&out.stderr);
            if !errors.trim().is_empty() {
                if !reply.is_empty() {
                    reply.push('\n');
                }
                reply.push_str(errors.trim_end());
            }
            (out.status.success(), reply)
        }
        Err(e) => (false, format!("could not run deskcrew: {e}")),
    }
}

fn base64(bytes: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(CHARS[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn is_dims(s: &str) -> bool {
    let Some((w, rest)) = s.split_once('x') else { return false };
    let h = rest.split_whitespace().next().unwrap_or("");
    !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()) && !h.is_empty() && h.bytes().all(|b| b.is_ascii_digit())
}

/// Extract the PNG path at the start of a `shot` reply (even if a parent folder contains `.png`).
fn picture_path(reply: &str) -> Option<&str> {
    let mut offset = 0;
    while let Some(pos) = reply[offset..].find(".png") {
        let end = offset + pos + 4;
        let after = &reply[end..];
        if after.is_empty() || (after.starts_with(' ') && is_dims(after.trim_start())) {
            return Some(&reply[..end]);
        }
        offset = end;
    }
    None
}

/// A shot's reply starts with the PNG's path: send the picture itself along with the line.
fn with_picture(reply: &str) -> Vec<Value> {
    let mut content = vec![text(reply)];
    if let Some(path) = picture_path(reply)
        && let Ok(bytes) = std::fs::read(path)
    {
        content.push(json!({ "type": "image", "data": base64(&bytes), "mimeType": "image/png" }));
    }
    content
}

fn call(agent: &str, params: &Value) -> Value {
    let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
    let empty = json!({});
    let arguments = params.get("arguments").unwrap_or(&empty);
    let args = match words(tool, arguments) {
        Ok(args) => args,
        Err(e) => return json!({ "content": [text(&e)], "isError": true }),
    };
    let (ok, reply) = run(agent, &args);
    let content = if tool == "shot" && ok { with_picture(&reply) } else { vec![text(&reply)] };
    json!({ "content": content, "isError": !ok })
}

pub fn serve() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let env_agent = std::env::var("DESKCREW_AGENT").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let mut agent = env_agent.clone().unwrap_or_else(|| "your agent".into());
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                let reply = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } });
                let _ = writeln!(stdout, "{reply}");
                let _ = stdout.flush();
                continue;
            }
        };
        // Notifications (no id) need no answer.
        let Some(id) = message.get("id").cloned() else { continue };
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => {
                if env_agent.is_none()
                    && let Some(name) = params.pointer("/clientInfo/name").and_then(Value::as_str)
                {
                    agent = agent_name(name);
                }
                let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(LATEST_PROTOCOL);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "deskcrew", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "deskcrew controls desktop apps in the background, without the person's mouse or focus. Read a window once with `see`, then run every step in one `do` call; the reply lists what changed."
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => Ok(call(&agent, &params)),
            _ => Err(json!({ "code": -32601, "message": format!("no method {method}") })),
        };
        let reply = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        let _ = writeln!(stdout, "{reply}");
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn tools_become_command_words() {
        assert_eq!(words("do", &json!({ "steps": "click OK", "window": "Calc" })).unwrap(), ["do", "-w", "Calc", "--", "click OK"]);
        assert_eq!(words("see", &json!({ "window": "Calc", "full": true })).unwrap(), ["see", "-f", "--", "Calc"]);
        assert_eq!(words("see", &json!({ "window": "--agent", "full": true })).unwrap(), ["see", "-f", "--", "--agent"]);
        assert_eq!(words("see", &json!({ "window": "--mouse" })).unwrap(), ["see", "--", "--mouse"]);
        assert_eq!(words("do", &json!({ "steps": "--agent", "window": "Calc" })).unwrap(), ["do", "-w", "Calc", "--", "--agent"]);
        assert!(words("see", &json!({ "window": "Calc", "find": "--agent" })).is_err());
        assert!(words("do", &json!({ "steps": "click OK", "window": "--agent" })).is_err());
        assert_eq!(words("shot", &json!({ "screen": true })).unwrap(), ["shot", "--screen"]);
        assert_eq!(words("shot", &json!({ "window": "--mouse" })).unwrap(), ["shot", "--", "--mouse"]);
        assert!(words("deskcrew", &json!({ "args": ["service"] })).is_err());
        assert!(words("deskcrew", &json!({ "args": ["do", "click 1,1 --mouse", "--agent", "Claude Code"] })).is_err());
        assert_eq!(words("deskcrew", &json!({ "args": ["type", "e3", "--", "--agent"] })).unwrap(), ["type", "e3", "--", "--agent"]);
        assert!(words("do", &json!({ "steps": "click OK" })).is_err());
    }

    #[test]
    fn clients_get_their_usual_names() {
        assert_eq!(agent_name("claude-ai"), "Claude");
        assert_eq!(agent_name("claude-code"), "Claude Code");
        assert_eq!(agent_name("codex-mcp-client"), "Codex");
        assert_eq!(agent_name("Something"), "Something");
    }

    #[test]
    fn picture_path_skips_png_in_parent_folder_names() {
        assert_eq!(
            picture_path(r"C:\Users\my.png.files\deskcrew\shots\Claude-w1.png 800x600 scale 1.00"),
            Some(r"C:\Users\my.png.files\deskcrew\shots\Claude-w1.png")
        );
        assert_eq!(
            picture_path(r"C:\Users\my.png files\deskcrew\shots\Claude-w1.png 800x600 scale 1.00"),
            Some(r"C:\Users\my.png files\deskcrew\shots\Claude-w1.png")
        );
        assert_eq!(
            picture_path("/tmp/folder with spaces/a-w0.png"),
            Some("/tmp/folder with spaces/a-w0.png")
        );
        assert_eq!(picture_path("unchanged"), None);
        assert_eq!(words("shot", &json!({ "region": "10,20,300,400" })).unwrap(), ["shot", "--region", "10,20,300,400"]);
    }
}
