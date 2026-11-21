// Wayland desktop portal integration (`org.freedesktop.portal.RemoteDesktop` v2,
// `org.freedesktop.portal.ScreenCast` v6, and `org.freedesktop.portal.Screenshot`) over the
// session D-Bus. Used on pure Wayland sessions where X11/XWayland is unavailable, persisting
// `restore_token` so the user only consents once.
use super::x::{Button, Chord, keysym_of};
use crate::shot::Frame;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue, Value};
use zbus::MatchRule;

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";
const SCREEN_CAST: &str = "org.freedesktop.portal.ScreenCast";
const SCREENSHOT: &str = "org.freedesktop.portal.Screenshot";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

// Device bitmask for RemoteDesktop.SelectDevices: keyboard (1) | pointer (2) | touchscreen (4).
const DEVICE_TYPES_ALL: u32 = 1 | 2 | 4;
// Persist mode 2: persist until explicitly revoked by the user.
const PERSIST_UNTIL_REVOKED: u32 = 2;
// ScreenCast source type 1 = MONITOR; cursor_mode 1 = HIDDEN (keeps user cursor out of OCR).
const SOURCE_MONITOR: u32 = 1;
const CURSOR_HIDDEN: u32 = 1;

const KEY_GAP: Duration = Duration::from_millis(12);
const PORTAL_CONSENT_TIMEOUT: Duration = Duration::from_secs(30);
const PORTAL_QUICK_TIMEOUT: Duration = Duration::from_secs(10);
const GST_FRAME_TIMEOUT: Duration = Duration::from_secs(3);

static TOKEN_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
struct PortalStream {
    id: u32,
    pos: (i32, i32),
    size: (i32, i32),
}

struct PortalSession {
    conn: Connection,
    session_handle: String,
    streams: Vec<PortalStream>,
}

enum PortalErr {
    SessionLost(String),
    Rejected(String),
}

static SESSION: Mutex<Option<PortalSession>> = Mutex::new(None);
/// Serializes `open_session()` without holding `SESSION` across the interactive consent prompt,
/// so read-only callers (like `screenshot_from_screencast`) are never stalled by another agent.
static OPEN_LOCK: Mutex<()> = Mutex::new(());

fn next_token(prefix: &str) -> String {
    let n = TOKEN_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("deskcrew_{prefix}_{}_{n}", std::process::id())
}

/// Predict the D-Bus object path of a portal Request from our connection's unique name and token,
/// so we can subscribe to `Response` BEFORE calling the portal method (avoiding any race).
fn request_path(unique_name: &str, token: &str) -> String {
    let clean = unique_name.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/request/{clean}/{token}")
}

fn token_file() -> Option<PathBuf> {
    if let Ok(state) = std::env::var("XDG_STATE_HOME")
        && !state.is_empty()
    {
        return Some(PathBuf::from(state).join("deskcrew").join("portal-restore-token"));
    }
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".local").join("state").join("deskcrew").join("portal-restore-token"))
}

fn load_restore_token() -> Option<String> {
    let path = token_file()?;
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn save_restore_token(token: &str) {
    let Some(path) = token_file() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path) {
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let _ = file.write_all(token.as_bytes());
    }
}

/// Wait for the `org.freedesktop.portal.Request::Response` signal on `req_path` after running
/// `send`, with a hard timeout so a stalled or dismissed portal dialog can never hang the service.
fn portal_request<F>(conn: &Connection, token: &str, timeout: Duration, send: F) -> Result<HashMap<String, OwnedValue>, String>
where
    F: FnOnce() -> Result<OwnedObjectPath, String>,
{
    let unique = conn.unique_name().ok_or("D-Bus connection has no unique name")?.as_str().to_string();
    let expected = request_path(&unique, token);
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(REQUEST_IFACE)
        .map_err(|e| e.to_string())?
        .member("Response")
        .map_err(|e| e.to_string())?
        .path(expected.as_str())
        .map_err(|e| e.to_string())?
        .build();
    let mut signals = MessageIterator::for_match_rule(rule, conn, Some(8)).map_err(|e| format!("could not listen for portal response: {e}"))?;
    let actual = send()?;
    // If the portal returned a different path than predicted (very old portal versions), re-subscribe.
    if actual.as_str() != expected {
        let fallback_rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(REQUEST_IFACE)
            .map_err(|e| e.to_string())?
            .member("Response")
            .map_err(|e| e.to_string())?
            .path(actual.as_str())
            .map_err(|e| e.to_string())?
            .build();
        signals = MessageIterator::for_match_rule(fallback_rule, conn, Some(8)).map_err(|e| format!("could not listen for portal response: {e}"))?;
    }
    let (tx, rx) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let _ = tx.send(signals.next());
    });
    let msg = match rx.recv_timeout(timeout) {
        Ok(Some(Ok(msg))) => msg,
        Ok(Some(Err(e))) => return Err(format!("desktop portal response failed: {e}")),
        Ok(None) => return Err("desktop portal closed before answering".into()),
        Err(_) => {
            // Close the D-Bus connection so the blocked `signals.next()` thread wakes up and exits
            // immediately instead of leaking a thread, match rule, and connection.
            let _ = conn.clone().close();
            let _ = waiter.join();
            return Err("desktop portal timed out waiting for a response".into());
        }
    };
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(|e| format!("unexpected portal response: {e}"))?;
    match code {
        0 => Ok(results),
        1 => Err("desktop portal request was cancelled by the person".into()),
        other => Err(format!("desktop portal refused the request (code {other})")),
    }
}

fn parse_pair(props: &HashMap<String, OwnedValue>, key: &str) -> Option<(i32, i32)> {
    let val = props.get(key)?.try_clone().ok()?;
    <(i32, i32)>::try_from(val).ok()
}

fn open_session() -> Result<PortalSession, String> {
    let conn = Connection::session().map_err(|e| format!("no D-Bus session ({e}): Wayland desktop portal unavailable"))?;

    // 1. RemoteDesktop.CreateSession
    let create_tok = next_token("rd_create");
    let session_tok = next_token("rd_session");
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("handle_token", Value::from(create_tok.as_str()));
    opts.insert("session_handle_token", Value::from(session_tok.as_str()));
    let created = portal_request(&conn, &create_tok, PORTAL_QUICK_TIMEOUT, || {
        conn.call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(REMOTE_DESKTOP), "CreateSession", &(opts,))
            .and_then(|m| m.body().deserialize::<OwnedObjectPath>())
            .map_err(|e| format!("Wayland RemoteDesktop portal unavailable ({e})"))
    })?;
    let session_handle: String = created
        .get("session_handle")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| String::try_from(v).ok())
        .ok_or("RemoteDesktop portal returned no session handle")?;

    // 2. RemoteDesktop.SelectDevices (with persist_mode=2 and cached restore_token if present)
    let dev_tok = next_token("rd_dev");
    let restore = load_restore_token();
    let mut dev_opts: HashMap<&str, Value<'_>> = HashMap::new();
    dev_opts.insert("handle_token", Value::from(dev_tok.as_str()));
    dev_opts.insert("types", Value::from(DEVICE_TYPES_ALL));
    dev_opts.insert("persist_mode", Value::from(PERSIST_UNTIL_REVOKED));
    if let Some(ref tok) = restore {
        dev_opts.insert("restore_token", Value::from(tok.as_str()));
    }
    let session_obj = zbus::zvariant::ObjectPath::try_from(session_handle.as_str()).map_err(|e| e.to_string())?;
    portal_request(&conn, &dev_tok, PORTAL_QUICK_TIMEOUT, || {
        conn.call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(REMOTE_DESKTOP), "SelectDevices", &(&session_obj, dev_opts))
            .and_then(|m| m.body().deserialize::<OwnedObjectPath>())
            .map_err(|e| format!("RemoteDesktop.SelectDevices failed: {e}"))
    })?;

    // 3. Optional ScreenCast.SelectSources on the same session so Start gives us PipeWire stream
    //    IDs + monitor geometry for absolute pointer positioning (`NotifyPointerMotionAbsolute`).
    //    Do NOT pass `restore_token` a second time here: per xdg-desktop-portal spec, the combined
    //    RemoteDesktop+ScreenCast session token is supplied once via SelectDevices.
    let sc_tok = next_token("sc_src");
    let mut sc_opts: HashMap<&str, Value<'_>> = HashMap::new();
    sc_opts.insert("handle_token", Value::from(sc_tok.as_str()));
    sc_opts.insert("types", Value::from(SOURCE_MONITOR));
    sc_opts.insert("multiple", Value::from(true));
    sc_opts.insert("cursor_mode", Value::from(CURSOR_HIDDEN));
    let _ = portal_request(&conn, &sc_tok, PORTAL_QUICK_TIMEOUT, || {
        conn.call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(SCREEN_CAST), "SelectSources", &(&session_obj, sc_opts))
            .and_then(|m| m.body().deserialize::<OwnedObjectPath>())
            .map_err(|e| e.to_string())
    });

    // 4. RemoteDesktop.Start (may prompt the person for one-time consent)
    let start_tok = next_token("rd_start");
    let mut start_opts: HashMap<&str, Value<'_>> = HashMap::new();
    start_opts.insert("handle_token", Value::from(start_tok.as_str()));
    let started = portal_request(&conn, &start_tok, PORTAL_CONSENT_TIMEOUT, || {
        conn.call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(REMOTE_DESKTOP), "Start", &(&session_obj, "", start_opts))
            .and_then(|m| m.body().deserialize::<OwnedObjectPath>())
            .map_err(|e| format!("RemoteDesktop.Start failed: {e}"))
    })?;

    if let Some(new_token) = started
        .get("restore_token")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| String::try_from(v).ok())
        .filter(|t| !t.is_empty())
    {
        save_restore_token(&new_token);
    }

    // Extract ScreenCast streams `a(ua{sv})` along with each monitor's logical position and size.
    let streams: Vec<PortalStream> = started
        .get("streams")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<(u32, HashMap<String, OwnedValue>)>::try_from(v).ok())
        .map(|list| {
            list.into_iter()
                .map(|(id, props)| PortalStream {
                    id,
                    pos: parse_pair(&props, "position").unwrap_or((0, 0)),
                    size: parse_pair(&props, "size").unwrap_or((0, 0)),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(PortalSession { conn, session_handle, streams })
}

fn with_session<T>(work: impl FnOnce(&PortalSession) -> Result<T, PortalErr>) -> Result<T, String> {
    if SESSION.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        let _opening = OPEN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if SESSION.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
            let opened = open_session()?;
            *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened);
        }
    }
    let mut slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let Some(session) = slot.as_ref() else {
        return Err("Wayland RemoteDesktop session is not open".into());
    };
    match work(session) {
        Ok(val) => Ok(val),
        Err(PortalErr::SessionLost(err)) => {
            // Reset cached session only on D-Bus transport/session failures so logical errors
            // (like missing monitor stream) never cause an infinite consent-dialog loop.
            if let Some(old) = slot.take() {
                let _ = old.conn.close();
            }
            Err(err)
        }
        Err(PortalErr::Rejected(err)) => Err(err),
    }
}

/// Map a global screen point `(px, py)` onto the matching PipeWire monitor stream's local space.
fn stream_point(streams: &[PortalStream], px: f64, py: f64) -> Option<(u32, f64, f64)> {
    if let Some(s) = streams.iter().find(|s| {
        s.size.0 > 0
            && s.size.1 > 0
            && px >= s.pos.0 as f64
            && py >= s.pos.1 as f64
            && px < (s.pos.0 + s.size.0) as f64
            && py < (s.pos.1 + s.size.1) as f64
    }) {
        return Some((s.id, px - s.pos.0 as f64, py - s.pos.1 as f64));
    }
    let first = streams.first()?;
    Some((first.id, px - first.pos.0 as f64, py - first.pos.1 as f64))
}

/// Linux evdev button code for `org.freedesktop.portal.RemoteDesktop.NotifyPointerButton`.
pub fn evdev_button(button: Button) -> i32 {
    match button {
        Button::Left => 0x110,    // BTN_LEFT
        Button::Right => 0x111,   // BTN_RIGHT
        Button::Middle => 0x112,  // BTN_MIDDLE
        Button::Back => 0x113,    // BTN_SIDE
        Button::Forward => 0x114, // BTN_EXTRA
    }
}

fn notify_move(session: &PortalSession, px: f64, py: f64) -> Result<(), PortalErr> {
    let (stream, lx, ly) = stream_point(&session.streams, px, py)
        .ok_or_else(|| PortalErr::Rejected("Wayland RemoteDesktop session has no monitor stream for absolute pointer movement".into()))?;
    let path = zbus::zvariant::ObjectPath::try_from(session.session_handle.as_str()).map_err(|e| PortalErr::SessionLost(e.to_string()))?;
    let opts: HashMap<&str, Value<'_>> = HashMap::new();
    session
        .conn
        .call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(REMOTE_DESKTOP), "NotifyPointerMotionAbsolute", &(&path, opts, stream, lx, ly))
        .map_err(|e| PortalErr::SessionLost(format!("RemoteDesktop pointer move failed: {e}")))?;
    Ok(())
}

fn notify_button(session: &PortalSession, button: Button, down: bool) -> Result<(), PortalErr> {
    let path = zbus::zvariant::ObjectPath::try_from(session.session_handle.as_str()).map_err(|e| PortalErr::SessionLost(e.to_string()))?;
    let opts: HashMap<&str, Value<'_>> = HashMap::new();
    let state: u32 = if down { 1 } else { 0 };
    session
        .conn
        .call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(REMOTE_DESKTOP), "NotifyPointerButton", &(&path, opts, evdev_button(button), state))
        .map_err(|e| PortalErr::SessionLost(format!("RemoteDesktop pointer button failed: {e}")))?;
    Ok(())
}

fn notify_keysym(session: &PortalSession, keysym: u32, down: bool) -> Result<(), PortalErr> {
    let path = zbus::zvariant::ObjectPath::try_from(session.session_handle.as_str()).map_err(|e| PortalErr::SessionLost(e.to_string()))?;
    let opts: HashMap<&str, Value<'_>> = HashMap::new();
    let state: u32 = if down { 1 } else { 0 };
    session
        .conn
        .call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(REMOTE_DESKTOP), "NotifyKeyboardKeysym", &(&path, opts, keysym as i32, state))
        .map_err(|e| PortalErr::SessionLost(format!("RemoteDesktop keysym failed: {e}")))?;
    Ok(())
}

pub fn hover(px: i32, py: i32) -> Result<(), String> {
    with_session(|s| notify_move(s, px as f64, py as f64))
}

pub fn click(px: i32, py: i32, button: Button, count: u32, hold: Duration, modifiers: &[u32]) -> Result<(), String> {
    with_session(|s| {
        notify_move(s, px as f64, py as f64)?;
        let mut pressed_mods = Vec::with_capacity(modifiers.len());
        let mut button_down = false;
        let res = (|| -> Result<(), PortalErr> {
            for &m in modifiers {
                notify_keysym(s, m, true)?;
                pressed_mods.push(m);
            }
            for _ in 0..count.max(1) {
                notify_button(s, button, true)?;
                button_down = true;
                thread::sleep(hold.max(KEY_GAP));
                notify_button(s, button, false)?;
                button_down = false;
                thread::sleep(KEY_GAP);
            }
            Ok(())
        })();
        if button_down {
            let _ = notify_button(s, button, false);
        }
        for m in pressed_mods.into_iter().rev() {
            let _ = notify_keysym(s, m, false);
        }
        res
    })
}

pub fn wheel(px: i32, py: i32, notches: i32, sideways: bool) -> Result<(), String> {
    with_session(|s| {
        notify_move(s, px as f64, py as f64)?;
        let path = zbus::zvariant::ObjectPath::try_from(s.session_handle.as_str()).map_err(|e| PortalErr::SessionLost(e.to_string()))?;
        let opts: HashMap<&str, Value<'_>> = HashMap::new();
        // Portal axis: 0 = vertical (positive = down), 1 = horizontal (positive = right).
        let axis: u32 = if sideways { 1 } else { 0 };
        let steps: i32 = if sideways { notches } else { -notches };
        s.conn
            .call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(REMOTE_DESKTOP), "NotifyPointerAxisDiscrete", &(&path, opts, axis, steps))
            .map_err(|e| PortalErr::SessionLost(format!("RemoteDesktop scroll failed: {e}")))?;
        Ok(())
    })
}

pub fn key(chord: &Chord, hold: Duration) -> Result<(), String> {
    with_session(|s| {
        let mut pressed_mods = Vec::with_capacity(chord.modifiers.len());
        let mut key_down = None;
        let res = (|| -> Result<(), PortalErr> {
            for &m in &chord.modifiers {
                notify_keysym(s, m, true)?;
                pressed_mods.push(m);
            }
            if let Some(sym) = chord.key {
                notify_keysym(s, sym, true)?;
                key_down = Some(sym);
                thread::sleep(hold.max(KEY_GAP));
                notify_keysym(s, sym, false)?;
                key_down = None;
            } else if !hold.is_zero() {
                thread::sleep(hold);
            }
            Ok(())
        })();
        if let Some(sym) = key_down {
            let _ = notify_keysym(s, sym, false);
        }
        for m in pressed_mods.into_iter().rev() {
            let _ = notify_keysym(s, m, false);
        }
        res
    })
}

pub fn type_text(text: &str, stop: &dyn Fn() -> bool) -> Result<usize, String> {
    with_session(|s| {
        let mut typed = 0;
        for c in text.chars() {
            if stop() {
                break;
            }
            let sym = keysym_of(c);
            notify_keysym(s, sym, true)?;
            notify_keysym(s, sym, false)?;
            typed += 1;
            thread::sleep(crate::motion::KEY_EVERY.saturating_sub(KEY_GAP));
        }
        Ok(typed)
    })
}

pub fn stroke(path: &[(f32, f32)], button: Button, modifiers: &[u32], stop: &dyn Fn() -> bool) -> Result<bool, String> {
    let Some(&(fx, fy)) = path.first() else { return Ok(true) };
    with_session(|s| {
        let mut pressed_mods = Vec::with_capacity(modifiers.len());
        let mut button_down = false;
        let res = (|| -> Result<bool, PortalErr> {
            for &m in modifiers {
                notify_keysym(s, m, true)?;
                pressed_mods.push(m);
            }
            notify_move(s, fx as f64, fy as f64)?;
            notify_button(s, button, true)?;
            button_down = true;
            let mut finished = true;
            for &(px, py) in path.iter().skip(1) {
                if stop() {
                    finished = false;
                    break;
                }
                notify_move(s, px as f64, py as f64)?;
                thread::sleep(Duration::from_millis(4));
            }
            notify_button(s, button, false)?;
            button_down = false;
            Ok(finished)
        })();
        if button_down {
            let _ = notify_button(s, button, false);
        }
        for m in pressed_mods.into_iter().rev() {
            let _ = notify_keysym(s, m, false);
        }
        res
    })
}

/// Decode a `file:///...` URI returned by `org.freedesktop.portal.Screenshot` into a local path.
fn decode_file_uri(uri: &str) -> Result<PathBuf, String> {
    let raw = uri.strip_prefix("file://").ok_or_else(|| format!("portal returned unsupported URI: {uri}"))?;
    let mut bytes = Vec::with_capacity(raw.len());
    let src = raw.as_bytes();
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%'
            && i + 2 < src.len()
            && let Ok(hex) = std::str::from_utf8(&src[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex, 16)
        {
            bytes.push(val);
            i += 3;
            continue;
        }
        bytes.push(src[i]);
        i += 1;
    }
    let decoded = String::from_utf8(bytes).map_err(|_| "portal screenshot path was not valid UTF-8".to_string())?;
    Ok(PathBuf::from(decoded))
}

/// Decode a PNG byte stream into deskcrew's BGRA `Frame`.
fn decode_png_to_frame(data: &[u8]) -> Result<Frame, String> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("could not decode portal PNG: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("could not read portal PNG frame: {e}"))?;
    let src = &buf[..info.buffer_size()];
    let (w, h) = (info.width, info.height);
    let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
    match info.color_type {
        png::ColorType::Rgba => {
            for (dst, px) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
                dst[0] = px[2];
                dst[1] = px[1];
                dst[2] = px[0];
                dst[3] = 255;
            }
        }
        png::ColorType::Rgb => {
            for (dst, px) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<3>().0) {
                dst[0] = px[2];
                dst[1] = px[1];
                dst[2] = px[0];
                dst[3] = 255;
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for (dst, px) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<2>().0) {
                dst[0] = px[0];
                dst[1] = px[0];
                dst[2] = px[0];
                dst[3] = 255;
            }
        }
        png::ColorType::Grayscale => {
            for (dst, &v) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(src.iter()) {
                dst[0] = v;
                dst[1] = v;
                dst[2] = v;
                dst[3] = 255;
            }
        }
        other => return Err(format!("unsupported portal PNG color type: {other:?}")),
    }
    Ok(Frame { w, h, pixels })
}

/// Select the stream containing logical point `hint` (if given), falling back to the first stream.
fn stream_for_hint(streams: &[PortalStream], hint: Option<(i32, i32)>) -> Option<PortalStream> {
    if let Some((hx, hy)) = hint
        && let Some(&s) = streams.iter().find(|s| {
            s.size.0 > 0
                && s.size.1 > 0
                && hx >= s.pos.0
                && hy >= s.pos.1
                && hx < s.pos.0 + s.size.0
                && hy < s.pos.1 + s.size.1
        })
    {
        return Some(s);
    }
    streams.first().copied()
}

/// Grab one silent PNG frame from the active `ScreenCast` PipeWire node (obtained via
/// `OpenPipeWireRemote`) using `gst-launch-1.0 pipewiresrc`, avoiding the desktop flash/sound
/// triggered by `org.freedesktop.portal.Screenshot` on GNOME/KDE. Returns `(frame, origin_logical,
/// logical_units_per_pixel)` so HiDPI and multi-monitor clicks map accurately to logical coords.
fn screenshot_from_screencast(hint: Option<(i32, i32)>) -> Option<(Frame, (i32, i32), f32)> {
    const F_GETFD: i32 = 1;
    const F_SETFD: i32 = 2;
    const FD_CLOEXEC: i32 = 1;
    unsafe extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }

    let (pw_fd, stream): (OwnedFd, PortalStream) = {
        let slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        let session = slot.as_ref()?;
        let stream = stream_for_hint(&session.streams, hint)?;
        let path = zbus::zvariant::ObjectPath::try_from(session.session_handle.as_str()).ok()?;
        let opts: HashMap<&str, Value<'_>> = HashMap::new();
        let reply = session
            .conn
            .call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(SCREEN_CAST), "OpenPipeWireRemote", &(&path, opts))
            .ok()?;
        let fd: OwnedFd = reply.body().deserialize().ok()?;
        (fd, stream)
    };

    let raw_fd = pw_fd.as_raw_fd();
    let mut cmd = Command::new("gst-launch-1.0");
    cmd.args([
        "-q",
        "pipewiresrc",
        &format!("fd={raw_fd}"),
        &format!("path={}", stream.id),
        "num-buffers=1",
        "!",
        "videoconvert",
        "!",
        "pngenc",
        "!",
        "fdsink",
        "fd=1",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    // Clear FD_CLOEXEC in the child right before exec so `pipewiresrc` inherits the portal FD.
    unsafe {
        cmd.pre_exec(move || {
            let flags = fcntl(raw_fd, F_GETFD);
            if flags >= 0 {
                let _ = fcntl(raw_fd, F_SETFD, flags & !FD_CLOEXEC);
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= GST_FRAME_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().ok()?;
    if !status.is_some_and(|s| s.success()) || bytes.is_empty() {
        return None;
    }
    let frame = decode_png_to_frame(&bytes).ok()?;
    let units = if stream.size.0 > 0 && frame.w > 0 { stream.size.0 as f32 / frame.w as f32 } else { 1.0 };
    Some((frame, stream.pos, units))
}

/// Capture the Wayland desktop silently via the active `ScreenCast` PipeWire node when available,
/// falling back to `org.freedesktop.portal.Screenshot` (non-interactive) otherwise.
/// Returns `(frame, origin_logical, logical_units_per_pixel)`.
pub fn screenshot(hint: Option<(i32, i32)>) -> Result<(Frame, (i32, i32), f32), String> {
    if let Some(captured) = screenshot_from_screencast(hint) {
        return Ok(captured);
    }
    let conn = Connection::session().map_err(|e| format!("no D-Bus session ({e}): Wayland Screenshot portal unavailable"))?;
    let tok = next_token("shot");
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("handle_token", Value::from(tok.as_str()));
    opts.insert("modal", Value::from(false));
    opts.insert("interactive", Value::from(false));
    let results = portal_request(&conn, &tok, PORTAL_QUICK_TIMEOUT, || {
        conn.call_method(Some(PORTAL_BUS), PORTAL_PATH, Some(SCREENSHOT), "Screenshot", &("", opts))
            .and_then(|m| m.body().deserialize::<OwnedObjectPath>())
            .map_err(|e| format!("Wayland Screenshot portal unavailable ({e})"))
    })?;
    let uri: String = results
        .get("uri")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| String::try_from(v).ok())
        .ok_or("Screenshot portal returned no image URI")?;
    let path = decode_file_uri(&uri)?;
    if !path.is_absolute() {
        return Err(format!("portal returned non-absolute screenshot path: {}", path.display()));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("could not read portal screenshot {}: {e}", path.display()))?;
    let _ = std::fs::remove_file(&path);
    let frame = decode_png_to_frame(&bytes)?;
    let (origin, units) = {
        let slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(session) = slot.as_ref()
            && !session.streams.is_empty()
        {
            let min_x = session.streams.iter().map(|s| s.pos.0).min().unwrap_or(0);
            let min_y = session.streams.iter().map(|s| s.pos.1).min().unwrap_or(0);
            let max_x = session.streams.iter().map(|s| s.pos.0 + s.size.0).max().unwrap_or(0);
            let logical_w = max_x - min_x;
            let units = if logical_w > 0 && frame.w > 0 { logical_w as f32 / frame.w as f32 } else { 1.0 };
            ((min_x, min_y), units)
        } else {
            ((0, 0), 1.0)
        }
    };
    Ok((frame, origin, units))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_path_sanitizes_sender() {
        assert_eq!(request_path(":1.42", "tok_1"), "/org/freedesktop/portal/desktop/request/1_42/tok_1");
    }

    #[test]
    fn decode_file_uri_handles_percent_encoding() {
        let p = decode_file_uri("file:///tmp/my%20shot%231.png").unwrap();
        assert_eq!(p, PathBuf::from("/tmp/my shot#1.png"));
        assert!(decode_file_uri("https://example.com/a.png").is_err());
    }

    #[test]
    fn evdev_button_codes_match_linux_input() {
        assert_eq!(evdev_button(Button::Left), 0x110);
        assert_eq!(evdev_button(Button::Right), 0x111);
        assert_eq!(evdev_button(Button::Middle), 0x112);
        assert_eq!(evdev_button(Button::Back), 0x113);
        assert_eq!(evdev_button(Button::Forward), 0x114);
    }

    #[test]
    fn png_decode_converts_rgba_to_bgra_frame() {
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            // Pixel 0: red (255, 0, 0, 255), Pixel 1: blue (0, 0, 255, 255)
            writer.write_image_data(&[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        }
        let frame = decode_png_to_frame(&png_bytes).unwrap();
        assert_eq!((frame.w, frame.h), (2, 1));
        assert_eq!(&frame.pixels[0..4], &[0, 0, 255, 255]); // red in BGRA
        assert_eq!(&frame.pixels[4..8], &[255, 0, 0, 255]); // blue in BGRA
    }

    #[test]
    fn stream_point_translates_multi_monitor_coordinates() {
        let streams = vec![
            PortalStream { id: 10, pos: (0, 0), size: (1920, 1080) },
            PortalStream { id: 20, pos: (1920, 0), size: (2560, 1440) },
        ];
        assert_eq!(stream_point(&streams, 100.0, 200.0), Some((10, 100.0, 200.0)));
        assert_eq!(stream_point(&streams, 2020.0, 300.0), Some((20, 100.0, 300.0)));
        assert_eq!(stream_point(&[], 10.0, 10.0), None);
    }
}
