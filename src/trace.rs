// Sessions and traces: a session gathers the commands of one run, from one agent or several, into a
// timeline that can be read back with `trace` or exported as one HTML page, with a picture of the
// window before and after each action if asked for. Typed text and values are left out, as in the
// log; control names, window titles and pictures stay in, so a trace is private to the person.
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Past this many commands a session records no more; start a new one.
const MOST_EVENTS: usize = 20_000;
/// Past this many pictures a session takes no more.
const MOST_PICTURES: usize = 1_000;
const REPLY_LINES: usize = 30;
const SHOWN_DEFAULT: usize = 40;

/// Commands that change something in a window, which get a picture before and after.
const ACTIONS: [&str; 22] = [
    "click", "hover", "set", "type", "clear", "toggle", "check", "uncheck", "expand", "collapse", "down", "up", "key", "select", "scroll", "show", "drag", "draw", "do", "pick-file", "place", "close",
];

/// Commands whose whole reply is content from the screen, files or the web: only its length is kept.
const CONTENT: [&str; 7] = ["get", "fetch", "table", "run", "processes", "clip", "reg"];

pub const HELP: &str = "\
session start [name] [--shots]   record this agent's commands, and with --shots a picture before and after each action
session join ID     session end [ID]     session list     session gc   (deletes ended sessions)
trace [ID] [n]      the last n commands of a session     trace export [ID] [--out page.html]   one page to read or share
Other agents join with session join ID, --session ID, or DESKCREW_SESSION=ID. Traces leave typed text and values out, but keep names, titles and pictures.";

pub struct Session {
    dir: PathBuf,
    shots: bool,
}

fn root() -> PathBuf {
    crate::shot::home().join("sessions")
}

/// A name safe as a file name.
fn safe(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

fn current_file(who: &str) -> PathBuf {
    root().join("current").join(safe(who))
}

fn millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn manifest(id: &str) -> Result<(PathBuf, Value), String> {
    if id.is_empty() || safe(id) != id {
        return Err(format!("no session '{id}': session list shows them"));
    }
    let dir = root().join(id);
    let text = fs::read_to_string(dir.join("session.json")).map_err(|_| format!("no session '{id}': session list shows them"))?;
    let value = serde_json::from_str(&text).map_err(|e| format!("session '{id}' is damaged: {e}"))?;
    Ok((dir, value))
}

fn write_manifest(dir: &Path, value: &Value) -> Result<(), String> {
    fs::write(dir.join("session.json"), serde_json::to_string_pretty(value).unwrap_or_default()).map_err(|e| e.to_string())
}

/// Take `--session ID` out of the arguments.
fn take_flag(args: &mut Vec<String>) -> Option<String> {
    let at = args.iter().position(|a| a == "--session")?;
    let id = args.get(at + 1).cloned();
    args.drain(at..(at + 2).min(args.len()));
    id
}

/// The session this command belongs to: `--session ID`, then DESKCREW_SESSION, then the one this
/// agent started or joined. None when there is none, or it has ended.
pub fn session_of(who: &str, args: &mut Vec<String>) -> Option<Session> {
    let id = take_flag(args)
        .or_else(|| std::env::var("DESKCREW_SESSION").ok().filter(|v| !v.is_empty()))
        .or_else(|| fs::read_to_string(current_file(who)).ok().map(|s| s.trim().to_string()))?;
    let (dir, value) = manifest(&id).ok()?;
    if !value["ended"].is_null() {
        return None;
    }
    Some(Session { shots: value["shots"].as_bool().unwrap_or(false), dir })
}

/// The first word that is not a flag or a flag's value: the command.
fn verb(words: &[String]) -> &str {
    let valued = ["-w", "--window", "--agent", "--session", "--with", "--hold", "--ms", "--expect", "--risk", "--region", "--edge", "--from", "--find"];
    let mut skip = false;
    for word in words {
        if skip {
            skip = false;
        } else if valued.contains(&word.as_str()) {
            skip = true;
        } else if !word.starts_with('-') {
            return word;
        }
    }
    ""
}

fn events(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("trace.jsonl")).unwrap_or_default().lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

/// A reply with what was read or typed left out: values after `=`, and the whole of a reply that
/// is content, such as `get` or `fetch`.
/// Whether a command's whole reply is content from the screen, files, the web or the system,
/// such as `get`, `clip` or `fetch`, which records keep only the length of.
pub fn reads_content(words: &[String]) -> bool {
    let command = verb(words);
    let clip_read = command == "clip" && !words.iter().any(|w| w == "set" || w == "clear");
    let file_read = command == "file" && words.iter().any(|w| w == "read" || w == "find");
    // A do whose steps read content carries that content in its reply too.
    let reads = |step: &str| {
        let mut parts = step.split_whitespace();
        let first = parts.next().unwrap_or("");
        CONTENT.contains(&first) && (first != "clip" || parts.next().is_none_or(|next| next != "set" && next != "clear"))
    };
    let do_reads = command == "do" && words.iter().skip_while(|w| *w != "do").skip(1).any(|script| script.split(';').any(|step| reads(step.trim())));
    (CONTENT.contains(&command) && (command != "clip" || clip_read)) || file_read || do_reads
}

fn redact(words: &[String], reply: &str) -> String {
    if reads_content(words) {
        return format!("<{} characters>", reply.chars().count());
    }
    let mut lines: Vec<String> = reply.lines().take(REPLY_LINES).map(redact_line).collect();
    let total = reply.lines().count();
    if total > REPLY_LINES {
        lines.push(format!("... {} more lines", total - REPLY_LINES));
    }
    lines.join("\n")
}

/// A control's line with its value and its cells left out: both come after the first " =" or
/// " | ", while the role and name before them stay.
fn redact_line(line: &str) -> String {
    let value = line.find(" =").map(|at| (at, " =<"));
    let cells = line.find(" | ").map(|at| (at, " |<"));
    match [value, cells].into_iter().flatten().min_by_key(|(at, _)| *at) {
        Some((at, mark)) => format!("{}{mark}{} characters>", &line[..at], line[at + 2..].trim_start().chars().count()),
        None => line.to_string(),
    }
}

impl Session {
    /// A picture of the window this action works in, kept in the session, for actions only and
    /// only in a session started with --shots. Taken as a separate agent, so the agent's own last
    /// shot, which its x,y refer to, stays as it was.
    pub fn picture(&self, who: &str, args: &[String], when: &str) -> Option<String> {
        if !self.shots || !ACTIONS.contains(&verb(args)) {
            return None;
        }
        let pictures = self.dir.join("shots");
        if fs::read_dir(&pictures).map(|d| d.count()).unwrap_or(0) >= MOST_PICTURES {
            return None;
        }
        let window = args.iter().position(|a| a == "-w" || a == "--window").and_then(|at| args.get(at + 1)).cloned().or_else(|| {
            // The window this agent is working in carries a * in its list.
            let (_, listed) = crate::client::ask(who, &["windows".to_string()]).ok()?;
            listed.lines().find_map(|line| line.split_whitespace().next().filter(|first| first.ends_with('*')).map(|first| first.trim_end_matches('*').to_string()))
        })?;
        let (ok, reply) = crate::client::ask(&format!("{who} (trace)"), &["shot".to_string(), "-f".into(), "-w".into(), window]).ok()?;
        let end = reply.find(".png")? + 4;
        if !ok {
            return None;
        }
        // Agents sharing a session can act at once: the process and time keep their names apart.
        let number = events(&self.dir).len() + 1;
        let name = format!("{number:05}-{when}-{}-{}.png", std::process::id(), millis() as u64);
        fs::create_dir_all(&pictures).ok()?;
        fs::copy(&reply[..end], pictures.join(&name)).ok()?;
        Some(format!("shots/{name}"))
    }

    /// Add one command to the timeline.
    #[allow(clippy::too_many_arguments)]
    pub fn record(&self, who: &str, words: &[String], ok: bool, reply: &str, took: Duration, before: Option<&str>, after: Option<&str>) {
        let file = self.dir.join("trace.jsonl");
        let count = fs::read_to_string(&file).map(|t| t.lines().count()).unwrap_or(0);
        if count >= MOST_EVENTS {
            return;
        }
        let event = json!({
            "t": millis() as u64,
            "at": crate::log::now(),
            "agent": who,
            "command": crate::log::hidden(words),
            "ok": ok,
            "ms": took.as_millis() as u64,
            "reply": redact(words, reply),
            "before": before,
            "after": after,
        });
        if let Ok(mut out) = OpenOptions::new().create(true).append(true).open(&file) {
            let _ = writeln!(out, "{event}");
        }
    }
}

/// `deskcrew session ...`
pub fn session_command(who: &str, words: &[String]) -> Result<String, String> {
    let shots = words.iter().any(|w| w == "--shots");
    let plain: Vec<&String> = words.iter().filter(|w| !w.starts_with("--")).collect();
    match plain.first().map(|w| w.as_str()) {
        Some("start") => {
            let name = plain.get(1).map(|n| n.to_string()).unwrap_or_default();
            let stamp: String = crate::log::now().chars().filter(char::is_ascii_digit).collect();
            let id = format!("s{}-{}", &stamp[2..], std::process::id() % 10_000);
            let dir = root().join(&id);
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            write_manifest(&dir, &json!({ "id": id, "name": name, "started": crate::log::now(), "t": millis() as u64, "ended": null, "shots": shots, "by": who }))?;
            fs::create_dir_all(root().join("current")).map_err(|e| e.to_string())?;
            fs::write(current_file(who), &id).map_err(|e| e.to_string())?;
            let pictures = if shots { ", with a picture before and after each action" } else { "" };
            Ok(format!("started session {id}{pictures}. {who}'s commands go into it until session end. Other agents join with: deskcrew session join {id}"))
        }
        Some("join") => {
            let id = plain.get(1).ok_or("session join ID")?;
            let (_, value) = manifest(id)?;
            if !value["ended"].is_null() {
                return Err(format!("session {id} has ended"));
            }
            fs::create_dir_all(root().join("current")).map_err(|e| e.to_string())?;
            fs::write(current_file(who), id.as_str()).map_err(|e| e.to_string())?;
            Ok(format!("{who} joined session {id}"))
        }
        Some("end") => {
            let id = match plain.get(1) {
                Some(id) => id.to_string(),
                None => fs::read_to_string(current_file(who)).map(|s| s.trim().to_string()).map_err(|_| "no session to end: session end ID")?,
            };
            let (dir, mut value) = manifest(&id)?;
            if value["ended"].is_null() {
                value["ended"] = json!(crate::log::now());
                write_manifest(&dir, &value)?;
            }
            // Every agent that was in it leaves it.
            if let Ok(list) = fs::read_dir(root().join("current")) {
                for entry in list.flatten() {
                    if fs::read_to_string(entry.path()).is_ok_and(|s| s.trim() == id) {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
            let all = events(&dir);
            let failed = all.iter().filter(|e| e["ok"] == json!(false)).count();
            Ok(format!("ended session {id}: {} commands, {failed} failed. trace export {id} makes a page of it", all.len()))
        }
        Some("list") => {
            let mut rows = Vec::new();
            for entry in fs::read_dir(root()).into_iter().flatten().flatten() {
                let id = entry.file_name().to_string_lossy().into_owned();
                let Ok((dir, value)) = manifest(&id) else { continue };
                let state = if value["ended"].is_null() { "open".to_string() } else { "ended".to_string() };
                let name = value["name"].as_str().filter(|n| !n.is_empty()).map(|n| format!(" \"{n}\"")).unwrap_or_default();
                rows.push((value["t"].as_u64().unwrap_or(0), format!("{id}{name} {} {state}, {} commands", value["started"].as_str().unwrap_or(""), events(&dir).len())));
            }
            rows.sort();
            Ok(if rows.is_empty() { "no sessions yet: session start".into() } else { rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>().join("\n") })
        }
        Some("gc") => {
            let mut removed = Vec::new();
            for entry in fs::read_dir(root()).into_iter().flatten().flatten() {
                let id = entry.file_name().to_string_lossy().into_owned();
                if let Ok((dir, value)) = manifest(&id)
                    && !value["ended"].is_null()
                    && fs::remove_dir_all(&dir).is_ok()
                {
                    removed.push(id);
                }
            }
            Ok(if removed.is_empty() { "no ended sessions to delete".into() } else { format!("deleted {}", removed.join(", ")) })
        }
        _ => Err(HELP.into()),
    }
}

/// The session a `trace` command means: the one named, else this agent's, else the newest.
fn chosen(who: &str, named: Option<&String>) -> Result<(String, PathBuf, Value), String> {
    let id = match named {
        Some(id) => id.to_string(),
        None => match fs::read_to_string(current_file(who)) {
            Ok(id) => id.trim().to_string(),
            Err(_) => {
                let newest = fs::read_dir(root())
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| {
                        let id = e.file_name().to_string_lossy().into_owned();
                        manifest(&id).ok().map(|(_, v)| (v["t"].as_u64().unwrap_or(0), id))
                    })
                    .max();
                newest.map(|(_, id)| id).ok_or("no sessions yet: session start")?
            }
        },
    };
    let (dir, value) = manifest(&id)?;
    Ok((id, dir, value))
}

/// `deskcrew trace ...`
pub fn trace_command(who: &str, words: &[String]) -> Result<String, String> {
    let out = words.iter().position(|w| w == "--out").and_then(|at| words.get(at + 1)).cloned();
    let plain: Vec<&String> = words.iter().enumerate().filter(|(at, w)| !w.starts_with("--") && !(*at > 0 && words[at - 1] == "--out")).map(|(_, w)| w).collect();
    if plain.first().is_some_and(|w| w.as_str() == "export") {
        let (id, dir, value) = chosen(who, plain.get(1).copied())?;
        let page = export(&dir, &value, &events(&dir));
        let path = out.map(PathBuf::from).unwrap_or_else(|| dir.join("trace.html"));
        fs::write(&path, page).map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok(format!("{} (session {id}; it holds window names and pictures, so keep it private)", path.display()));
    }
    let count = plain.iter().find_map(|w| w.parse::<usize>().ok()).unwrap_or(SHOWN_DEFAULT);
    let named = plain.iter().find(|w| w.parse::<usize>().is_err()).copied();
    let (id, dir, value) = chosen(who, named)?;
    let all = events(&dir);
    let start = value["t"].as_u64().unwrap_or(0);
    let from = all.len().saturating_sub(count);
    let mut lines: Vec<String> = all[from..]
        .iter()
        .map(|e| {
            let at = e["t"].as_u64().unwrap_or(start).saturating_sub(start) as f64 / 1000.0;
            let first = e["reply"].as_str().unwrap_or("").lines().next().unwrap_or("");
            let pictures = if e["before"].is_string() { " [pictures]" } else { "" };
            format!(
                "+{at:.1}s {} {} {}ms {}  ->  {first}{pictures}",
                e["agent"].as_str().unwrap_or(""),
                if e["ok"] == json!(true) { "ok" } else { "failed" },
                e["ms"],
                e["command"].as_str().unwrap_or("")
            )
        })
        .collect();
    let state = if value["ended"].is_null() { "open" } else { "ended" };
    lines.insert(0, format!("session {id} ({state}), {} commands, from {}", all.len(), value["started"].as_str().unwrap_or("")));
    Ok(lines.join("\n"))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// One self-contained page: the timeline, with the pictures inside it.
fn export(dir: &Path, value: &Value, all: &[Value]) -> String {
    let start = value["t"].as_u64().unwrap_or(0);
    let picture = |e: &Value, key: &str| {
        e[key].as_str().and_then(|name| fs::read(dir.join(name)).ok()).map(|png| format!("<figure><img alt=\"{key}\" src=\"data:image/png;base64,{}\"><figcaption>{key}</figcaption></figure>", base64(&png))).unwrap_or_default()
    };
    let rows: String = all
        .iter()
        .map(|e| {
            let at = e["t"].as_u64().unwrap_or(start).saturating_sub(start) as f64 / 1000.0;
            let ok = e["ok"] == json!(true);
            format!(
                "<tr class=\"{}\"><td>+{at:.1}s</td><td>{}</td><td><code>{}</code></td><td>{}</td><td>{} ms</td><td><pre>{}</pre><div class=\"shots\">{}{}</div></td></tr>",
                if ok { "ok" } else { "failed" },
                escape(e["agent"].as_str().unwrap_or("")),
                escape(e["command"].as_str().unwrap_or("")),
                if ok { "ok" } else { "failed" },
                e["ms"],
                escape(e["reply"].as_str().unwrap_or("")),
                picture(e, "before"),
                picture(e, "after"),
            )
        })
        .collect();
    let failed = all.iter().filter(|e| e["ok"] == json!(false)).count();
    let name = value["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("deskcrew session");
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{title}</title><style>\
:root{{--bg:#fff;--fg:#1a1a1a;--muted:#666;--line:#ddd;--bad:#b42318;--card:#f6f6f6}}\
@media (prefers-color-scheme: dark){{:root{{--bg:#151515;--fg:#eee;--muted:#999;--line:#333;--bad:#f97066;--card:#1f1f1f}}}}\
body{{margin:0;padding:16px;background:var(--bg);color:var(--fg);font:14px/1.45 system-ui,sans-serif}}\
h1{{font-size:20px;margin:0 0 4px}}p{{color:var(--muted);margin:0 0 16px}}\
table{{border-collapse:collapse;width:100%}}td{{border-top:1px solid var(--line);padding:8px 6px;vertical-align:top}}\
tr.failed td:nth-child(4){{color:var(--bad);font-weight:600}}code,pre{{font:12px/1.4 ui-monospace,Consolas,monospace;white-space:pre-wrap;word-break:break-word;margin:0}}\
.shots{{display:flex;gap:8px;flex-wrap:wrap;margin-top:6px}}figure{{margin:0;background:var(--card);padding:4px;border-radius:6px}}\
img{{max-width:min(420px,90vw);display:block}}figcaption{{color:var(--muted);font-size:12px}}\
@media (max-width:700px){{td:nth-child(5){{display:none}}}}\
</style></head><body><h1>{title}</h1><p>Session {id}, started {started}{ended}. {count} commands, {failed} failed. Typed text and values are left out; names, titles and pictures are not, so keep this page private.</p>\
<table><tbody>{rows}</tbody></table></body></html>",
        title = escape(name),
        id = escape(value["id"].as_str().unwrap_or("")),
        started = escape(value["started"].as_str().unwrap_or("")),
        ended = value["ended"].as_str().map(|e| format!(", ended {}", escape(e))).unwrap_or_default(),
        count = all.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn values_and_content_are_left_out() {
        assert_eq!(redact(&words(&["set", "Name", "Ada"]), "ok value\n~ e1 edit #Box =Ada"), "ok value\n~ e1 edit #Box =<3 characters>");
        assert_eq!(redact(&words(&["get", "e3"]), "secret text"), "<11 characters>");
        assert_eq!(redact(&words(&["clip", "set", "pw"]), "ok"), "ok");
        assert_eq!(redact(&words(&["clip"]), "pw"), "<2 characters>");
        assert_eq!(redact(&words(&["-w", "Notes", "file", "read", "a.txt"]), "body"), "<4 characters>");
        assert_eq!(redact(&words(&["see"]), "e4 item \"Ada\" | ada@example.com | 3"), "e4 item \"Ada\" |<19 characters>");
        assert_eq!(redact(&words(&["do", "click e1; get e3"]), "ok
secret"), "<9 characters>");
        assert_eq!(redact(&words(&["reg", "get", "HKCU\\Key"]), "value"), "<5 characters>");
    }

    #[test]
    fn the_command_is_found_past_flags() {
        assert_eq!(verb(&words(&["-w", "Notes", "click", "Save"])), "click");
        assert_eq!(verb(&words(&["--real", "key", "enter"])), "key");
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }

    #[test]
    fn the_session_flag_is_taken_out() {
        let mut args = words(&["click", "Save", "--session", "s1", "-w", "Notes"]);
        assert_eq!(take_flag(&mut args), Some("s1".into()));
        assert_eq!(args, words(&["click", "Save", "-w", "Notes"]));
    }
}
