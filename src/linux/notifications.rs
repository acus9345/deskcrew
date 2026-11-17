// Desktop notifications on Linux. There is no common way to ask the notification server what it is
// showing, but every app sends its notifications the same way: a Notify call on the session bus.
// deskcrew watches those from the moment its service starts, so `notifications` lists what was
// sent since then, newest first.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::thread;
use std::time::Instant;
use zbus::zvariant::OwnedValue;

const KEPT: usize = 50;

struct Sent {
    app: String,
    summary: String,
    body: String,
    at: Instant,
}

static SEEN: Mutex<VecDeque<Sent>> = Mutex::new(VecDeque::new());
static WATCHING: Mutex<Option<Result<(), String>>> = Mutex::new(None);

/// Start watching the session bus for notifications, once.
pub fn watch() {
    let mut watching = WATCHING.lock().unwrap();
    if watching.is_some() {
        return;
    }
    *watching = Some(Ok(()));
    drop(watching);
    thread::spawn(|| {
        if let Err(problem) = listen() {
            *WATCHING.lock().unwrap() = Some(Err(problem));
        }
    });
}

fn listen() -> Result<(), String> {
    let conn = zbus::blocking::Connection::session().map_err(|e| format!("no D-Bus session ({e}), so no notifications can be seen"))?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::MethodCall)
        .interface("org.freedesktop.Notifications")
        .and_then(|b| b.member("Notify"))
        .map_err(|e| e.to_string())?
        .build();
    let refused = |e: &dyn std::fmt::Display| format!("the session bus would not let deskcrew watch notifications: {e}");
    let monitor = zbus::blocking::fdo::MonitoringProxy::new(&conn).map_err(|e| refused(&e))?;
    monitor.become_monitor(&[rule], 0).map_err(|e| refused(&e))?;
    for message in zbus::blocking::MessageIterator::from(&conn).flatten() {
        type Notify = (String, u32, String, String, String, Vec<String>, HashMap<String, OwnedValue>, i32);
        let Ok((app, _, _, summary, body, _, _, _)) = message.body().deserialize::<Notify>() else { continue };
        let mut seen = SEEN.lock().unwrap();
        seen.push_front(Sent { app, summary, body, at: Instant::now() });
        seen.truncate(KEPT);
    }
    Err("the session bus closed".into())
}

/// The notifications sent since the service started, newest first, one a line.
pub fn list() -> Result<String, String> {
    if let Some(Err(problem)) = WATCHING.lock().unwrap().as_ref() {
        return Err(problem.clone());
    }
    let seen = SEEN.lock().unwrap();
    if seen.is_empty() {
        return Ok("no notifications since deskcrew started watching".into());
    }
    Ok(seen
        .iter()
        .enumerate()
        .map(|(at, sent)| {
            let ago = sent.at.elapsed().as_secs();
            let body = if sent.body.is_empty() { String::new() } else { format!(": {}", sent.body.replace('\n', " ")) };
            format!("n{} {} ({ago}s ago) {}{body}", at + 1, if sent.app.is_empty() { "an app" } else { &sent.app }, sent.summary)
        })
        .collect::<Vec<_>>()
        .join("\n"))
}
