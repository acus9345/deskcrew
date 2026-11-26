// Chrome DevTools Protocol (CDP) browser control: read and act on web pages directly through
// their DOM instead of the OS accessibility tree. Pure Rust over loopback TCP (HTTP/1.1 +
// RFC 6455 WebSocket + serde_json) so it runs identically on Windows, macOS, and Linux without
// adding external dependencies, touching the person's cursor, or taking window focus.
use crate::{
    lines::{self, Parts, Seen},
    motion,
    shot::{self, Frame, Frames},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PROBE_TIMEOUT: Duration = Duration::from_millis(150);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const IO_TIMEOUT: Duration = Duration::from_secs(6);
const CALL_TIMEOUT: Duration = Duration::from_secs(8);
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(2);
const MAX_HTTP_BYTES: usize = 8 * 1024 * 1024;
const MAX_WS_BYTES: usize = 32 * 1024 * 1024;
const SETTLE_FIRST: Duration = Duration::from_millis(40);
const SETTLE_STEP: Duration = Duration::from_millis(50);
const SETTLE_LEAST: Duration = Duration::from_millis(120);
const SETTLE_MOST: Duration = Duration::from_millis(600);
const BETWEEN_STEPS: Duration = Duration::from_millis(30);
const WAIT_DEFAULT_MS: u64 = 10_000;
const WAIT_MOST_MS: u64 = 60_000;
const WAIT_STEP: Duration = Duration::from_millis(150);
const HOLD_MOST_MS: u64 = 10_000;
const MAX_GET_CHARS: usize = 20_000;
const MAX_TABLE_ROWS: usize = 500;
const MAX_TABLE_COLUMNS: usize = 20;
const MAX_CELL_CHARS: usize = 60;
const TABLE_CHARS: usize = 6000;

/// Verbs that can execute directly against a CDP page DOM.
pub const CDP_ACTIONS: [&str; 17] = [
    "click", "hover", "set", "type", "key", "check", "uncheck", "toggle", "expand", "collapse", "clear", "select", "scroll", "show", "drag", "draw", "wait",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Http { host: String, port: u16 },
    Ws { host: String, port: u16, path: String },
}

fn format_host_port(host: &str, port: u16) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

impl Endpoint {
    pub fn display(&self) -> String {
        match self {
            Endpoint::Http { host, port } => format!("http://{}", format_host_port(host, *port)),
            Endpoint::Ws { host, port, path } => format!("ws://{}{path}", format_host_port(host, *port)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetInfo {
    pub id: String,
    pub title: String,
    pub url: String,
    pub kind: String,
    pub ws_url: String,
}

#[derive(Clone, Debug, Default)]
pub struct ActionFlags {
    pub full: bool,
    pub all: bool,
    pub find: Option<String>,
    pub from: usize,
    pub right: bool,
    pub double: bool,
    pub triple: bool,
    pub middle: bool,
    pub back: bool,
    pub forward: bool,
    pub hold: Option<u64>,
    pub with: Option<String>,
    pub button: Option<String>,
    pub smooth: bool,
    pub fill: bool,
    pub close: bool,
    pub gone: bool,
    pub ms: Option<u64>,
    pub expect: Option<String>,
    pub force: bool,
    pub marks: bool,
    pub rulers: bool,
    pub changed: bool,
    pub edge: Option<u32>,
    pub region: Option<(i32, i32, i32, i32)>,
}

#[derive(Clone, Debug)]
pub struct ActionStep {
    pub flags: ActionFlags,
    pub words: Vec<String>,
}

/// True when a `-w` window spec explicitly names a CDP target (`cdp`, `cdp:...`, or `t<N>`).
pub fn is_direct_spec(spec: &str) -> bool {
    let lower = spec.to_ascii_lowercase();
    lower == "cdp"
        || lower.starts_with("cdp:")
        || lower.strip_prefix('t').and_then(|n| n.parse::<usize>().ok()).is_some_and(|n| n >= 1)
}

/// Parse a loopback CDP endpoint: a port (`"9222"`), `http://127.0.0.1:9222`, or
/// `ws://127.0.0.1:9222/devtools/page/...`. Non-loopback hosts are rejected.
pub fn parse_endpoint(raw: &str) -> Result<Endpoint, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("empty CDP endpoint".into());
    }
    if text.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err("invalid characters in CDP endpoint".into());
    }
    if let Ok(port) = text.parse::<u16>() {
        if port == 0 {
            return Err("CDP port must be non-zero".into());
        }
        return Ok(Endpoint::Http { host: "127.0.0.1".into(), port });
    }
    let (is_ws, rest) = if let Some(r) = text.strip_prefix("ws://") {
        (true, r)
    } else if let Some(r) = text.strip_prefix("http://") {
        (false, r)
    } else if text.starts_with("wss://") || text.starts_with("https://") {
        return Err("CDP uses local unencrypted http:// or ws:// on loopback only".into());
    } else {
        (false, text)
    };
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    let (host, port_str) = if let Some(bracketed) = authority.strip_prefix('[') {
        let end = bracketed.find(']').ok_or("malformed IPv6 host in CDP endpoint")?;
        let host = &bracketed[..end];
        let after = &bracketed[end + 1..];
        let port = after.strip_prefix(':').ok_or("CDP endpoint requires a port")?;
        (host, port)
    } else {
        authority.rsplit_once(':').ok_or("CDP endpoint requires host:port or a port number")?
    };
    if !is_loopback_host(host) {
        return Err(format!("refused: CDP only connects to loopback (127.0.0.1 / localhost / ::1), not '{host}'"));
    }
    let port: u16 = port_str.parse().map_err(|_| format!("invalid CDP port '{port_str}'"))?;
    if port == 0 {
        return Err("CDP port must be non-zero".into());
    }
    let normalized_host = if host.eq_ignore_ascii_case("localhost") { "localhost" } else { host };
    if is_ws {
        let ws_path = if path.is_empty() { "/".to_string() } else { path.to_string() };
        Ok(Endpoint::Ws { host: normalized_host.into(), port, path: ws_path })
    } else {
        Ok(Endpoint::Http { host: normalized_host.into(), port })
    }
}

fn is_loopback_host(host: &str) -> bool {
    host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost") || host == "::1"
}

/// Allocate a free loopback TCP port for `deskcrew launch <browser> --cdp`.
pub fn allocate_port() -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("could not bind local port: {e}"))?;
    listener.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

/// Dedicated profile directory for `deskcrew launch <browser> --cdp` (Chromium 136+ ignores
/// `--remote-debugging-port` on the default user-data-dir).
pub fn profile_dir(browser: &str) -> PathBuf {
    let slug: String = browser
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "browser" } else { slug };
    shot::home().join("cdp-profiles").join(slug)
}

/// Build Chromium launch arguments that enable CDP on `port` with a dedicated profile directory,
/// preserving any user-supplied arguments (and respecting an explicit `--remote-debugging-port`).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn chromium_cdp_args(existing_args: &str, browser: &str, port: u16) -> (String, u16) {
    if let Some(found) = extract_debugging_port(existing_args) {
        return (existing_args.to_string(), found);
    }
    let mut parts = Vec::new();
    parts.push(format!("--remote-debugging-port={port}"));
    if !existing_args.contains("--user-data-dir") {
        let dir = profile_dir(browser);
        let _ = fs::create_dir_all(&dir);
        parts.push(format!("--user-data-dir=\"{}\"", dir.display()));
    }
    if !existing_args.trim().is_empty() {
        parts.push(existing_args.trim().to_string());
    }
    (parts.join(" "), port)
}

/// Extract `--remote-debugging-port=N` (or `--remote-debugging-port N`) from launch arguments.
pub fn extract_debugging_port(args: &str) -> Option<u16> {
    let mut iter = args.split_whitespace();
    while let Some(tok) = iter.next() {
        if let Some(val) = tok.strip_prefix("--remote-debugging-port=") {
            if let Ok(port) = val.trim_matches('"').parse::<u16>()
                && port != 0
            {
                return Some(port);
            }
        } else if tok == "--remote-debugging-port"
            && let Some(next) = iter.next()
            && let Ok(port) = next.trim_matches('"').parse::<u16>()
            && port != 0
        {
            return Some(port);
        }
    }
    None
}

fn normalize_app_stem(app: &str) -> String {
    let lower = app.trim().to_ascii_lowercase();
    let base = lower.rsplit(['\\', '/']).next().unwrap_or(&lower);
    base.strip_suffix(".exe").or_else(|| base.strip_suffix(".app")).unwrap_or(base).to_string()
}

/// Recognize Chromium/Electron browser executable or app names across Windows, macOS, and Linux.
pub fn is_browser_app(app: &str) -> bool {
    let stem = normalize_app_stem(app);
    const EXACT: &[&str] = &[
        "chrome",
        "google chrome",
        "google-chrome",
        "google-chrome-stable",
        "google-chrome-beta",
        "google-chrome-unstable",
        "chromium",
        "chromium-browser",
        "msedge",
        "microsoft edge",
        "microsoft-edge",
        "microsoft-edge-stable",
        "microsoft-edge-beta",
        "microsoft-edge-dev",
        "edge",
        "brave",
        "brave browser",
        "brave-browser",
        "vivaldi",
        "vivaldi-stable",
        "opera",
        "arc",
        "electron",
    ];
    if EXACT.contains(&stem.as_str()) {
        return true;
    }
    const PREFIXES: &[&str] = &[
        "google chrome ",
        "google-chrome-",
        "microsoft edge ",
        "microsoft-edge-",
        "brave browser ",
        "brave-browser-",
        "chromium-",
        "vivaldi-",
    ];
    PREFIXES.iter().any(|p| stem.starts_with(p))
}

/// Strip standard browser title suffixes (including Edge's zero-width space `\u{200b}`).
pub fn strip_browser_suffix(title: &str) -> String {
    let cleaned: String = title.chars().filter(|&c| c != '\u{200b}' && c != '\u{feff}').collect();
    let mut cur = cleaned.trim();
    const SUFFIXES: &[&str] = &[
        " - Google Chrome",
        " - Microsoft Edge",
        " - Brave",
        " - Chromium",
        " - Vivaldi",
        " - Opera",
        " - Arc",
        " — Google Chrome",
        " — Microsoft Edge",
        " — Brave",
        " — Chromium",
    ];
    for suffix in SUFFIXES {
        if let Some(stripped) = cur.strip_suffix(suffix) {
            cur = stripped.trim();
            // Edge often appends " - Work" or " - Personal" or " and 3 more pages" before "- Microsoft Edge".
            if let Some(idx) = cur.rfind(" and ")
                && cur[idx..].ends_with(" more pages")
            {
                cur = cur[..idx].trim();
            } else if let Some(idx) = cur.rfind(" and ")
                && cur[idx..].ends_with(" more page")
            {
                cur = cur[..idx].trim();
            }
            break;
        }
    }
    cur.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Match an OS window title against CDP page/webview targets.
pub fn match_page_target<'a>(window_title: &str, targets: &'a [TargetInfo], allow_single_fallback: bool) -> Option<&'a TargetInfo> {
    let pages: Vec<&TargetInfo> = targets
        .iter()
        .filter(|t| matches!(t.kind.as_str(), "page" | "webview") && !t.ws_url.is_empty())
        .collect();
    if pages.is_empty() {
        return None;
    }
    let needle = strip_browser_suffix(window_title).to_ascii_lowercase();
    if !needle.is_empty() {
        if let Some(exact) = pages.iter().find(|t| strip_browser_suffix(&t.title).to_ascii_lowercase() == needle) {
            return Some(*exact);
        }
        // Prefer longest matching non-empty title where either side contains the other.
        let best = pages
            .iter()
            .filter_map(|t| {
                let cand = strip_browser_suffix(&t.title).to_ascii_lowercase();
                if cand.is_empty() {
                    return None;
                }
                if needle.contains(&cand) || cand.contains(&needle) {
                    Some((cand.len(), *t))
                } else {
                    None
                }
            })
            .max_by_key(|(len, _)| *len);
        if let Some((_, target)) = best {
            return Some(target);
        }
    }
    if allow_single_fallback && pages.len() == 1 {
        return Some(pages[0]);
    }
    None
}

// ---------------------------------------------------------------------------
// Minimal RFC 3174 SHA-1 + Base64 (used for RFC 6455 handshake & PNG decode)
// ---------------------------------------------------------------------------

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for b in input.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err("invalid base64 data from CDP".into()),
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1u32 << bits) - 1;
        }
    }
    Ok(out)
}

pub fn ws_accept_key(sec_websocket_key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut buf = String::with_capacity(sec_websocket_key.len() + GUID.len());
    buf.push_str(sec_websocket_key.trim());
    buf.push_str(GUID);
    base64_encode(&sha1(buf.as_bytes()))
}

// ---------------------------------------------------------------------------
// Loopback TCP + HTTP/1.1 + RFC 6455 WebSocket client
// ---------------------------------------------------------------------------

fn connect_loopback(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    if !is_loopback_host(host) {
        return Err(format!("refused: non-loopback CDP host '{host}'"));
    }
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve {host}:{port}: {e}"))?
        .filter(|addr| addr.ip().is_loopback())
        .collect();
    if addrs.is_empty() {
        return Err(format!("no loopback address for {host}:{port}"));
    }
    let mut last_err = String::new();
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
                let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
                return Ok(stream);
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!("could not connect to CDP at {host}:{port}: {last_err}"))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let crlf = data.windows(2).position(|w| w == b"\r\n").ok_or("malformed chunked HTTP body")?;
        let line = std::str::from_utf8(&data[..crlf]).map_err(|_| "non-UTF8 chunk size")?;
        let size_hex = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| "invalid chunk size")?;
        data = &data[crlf + 2..];
        if size == 0 {
            break;
        }
        if out.len().saturating_add(size) > MAX_HTTP_BYTES {
            return Err("CDP HTTP response too large".into());
        }
        if data.len() < size + 2 {
            return Err("truncated chunked HTTP body".into());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
    Ok(out)
}

fn http_get(host: &str, port: u16, path: &str, timeout: Duration) -> Result<String, String> {
    let mut stream = connect_loopback(host, port, timeout)?;
    let _ = stream.set_read_timeout(Some(timeout));
    let host_hdr = format_host_port(host, port);
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host_hdr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| format!("CDP HTTP write failed: {e}"))?;
    let mut raw = Vec::new();
    let mut tmp = [0u8; 8192];
    let mut header_end: Option<usize> = None;
    let mut content_len: Option<usize> = None;
    let mut chunked = false;
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                if raw.len() + n > MAX_HTTP_BYTES {
                    return Err("CDP HTTP response exceeded size limit".into());
                }
                raw.extend_from_slice(&tmp[..n]);
                if header_end.is_none()
                    && let Some(end) = find_header_end(&raw)
                {
                    header_end = Some(end);
                    if let Ok(headers_str) = std::str::from_utf8(&raw[..end]) {
                        for line in headers_str.lines().skip(1) {
                            if let Some((k, v)) = line.split_once(':') {
                                if k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked") {
                                    chunked = true;
                                } else if k.eq_ignore_ascii_case("content-length") {
                                    content_len = v.trim().parse().ok();
                                }
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    let body = &raw[end..];
                    if let Some(len) = content_len
                        && body.len() >= len
                    {
                        break;
                    }
                    if chunked && body.ends_with(b"0\r\n\r\n") {
                        break;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                if header_end.is_some() {
                    break;
                }
                return Err(format!("CDP HTTP read timed out: {e}"));
            }
            Err(e) => return Err(format!("CDP HTTP read failed: {e}")),
        }
    }
    let header_end = header_end.ok_or("incomplete HTTP response from CDP")?;
    let headers_str = std::str::from_utf8(&raw[..header_end]).map_err(|_| "non-UTF8 HTTP headers from CDP")?;
    let status_line = headers_str.lines().next().unwrap_or("");
    let code = status_line.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
    if code != 200 {
        return Err(format!("CDP HTTP {path} returned status {code}"));
    }
    let body = &raw[header_end..];
    let bytes = if chunked {
        decode_chunked(body)?
    } else if let Some(len) = content_len {
        body[..body.len().min(len)].to_vec()
    } else {
        body.to_vec()
    };
    String::from_utf8(bytes).map_err(|_| "CDP HTTP body was not UTF-8".into())
}

static NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_mask_bytes() -> [u8; 4] {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
    let c = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mix = t.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ c.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let bytes = (mix as u32).to_le_bytes();
    if bytes == [0, 0, 0, 0] { [0x5A, 0xA5, 0x3C, 0xC3] } else { bytes }
}

fn next_ws_key() -> String {
    let mut bytes = [0u8; 16];
    for chunk in bytes.as_chunks_mut::<4>().0 {
        chunk.copy_from_slice(&next_mask_bytes());
    }
    base64_encode(&bytes)
}

struct WsClient {
    stream: TcpStream,
    buf: Vec<u8>,
    pos: usize,
}

impl WsClient {
    fn connect(host: &str, port: u16, path: &str) -> Result<Self, String> {
        let mut stream = connect_loopback(host, port, CONNECT_TIMEOUT)?;
        let key = next_ws_key();
        let expected_accept = ws_accept_key(&key);
        let host_hdr = format_host_port(host, port);
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host_hdr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).map_err(|e| format!("CDP WebSocket handshake write failed: {e}"))?;
        let mut raw = Vec::new();
        let mut tmp = [0u8; 4096];
        let header_end = loop {
            let n = match stream.read(&mut tmp) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("CDP WebSocket handshake read failed: {e}")),
            };
            if n == 0 {
                return Err("CDP closed socket during WebSocket handshake".into());
            }
            raw.extend_from_slice(&tmp[..n]);
            if let Some(end) = find_header_end(&raw) {
                break end;
            }
            if raw.len() > 64 * 1024 {
                return Err("CDP WebSocket handshake headers too large".into());
            }
        };
        let headers = std::str::from_utf8(&raw[..header_end]).map_err(|_| "invalid UTF-8 in WebSocket handshake")?;
        let status = headers.lines().next().unwrap_or("");
        if !status.contains(" 101 ") {
            return Err(format!("CDP WebSocket upgrade failed: {status}"));
        }
        let accept = headers
            .lines()
            .skip(1)
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                k.eq_ignore_ascii_case("sec-websocket-accept").then(|| v.trim())
            })
            .unwrap_or("");
        if accept != expected_accept {
            return Err("CDP WebSocket Sec-WebSocket-Accept mismatch".into());
        }
        let leftover = raw[header_end..].to_vec();
        Ok(WsClient { stream, buf: leftover, pos: 0 })
    }

    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut header = Vec::with_capacity(14);
        header.push(0x80 | (opcode & 0x0F));
        let len = payload.len();
        if len < 126 {
            header.push(0x80 | (len as u8));
        } else if len <= 65_535 {
            header.push(0x80 | 126);
            header.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            header.push(0x80 | 127);
            header.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask = next_mask_bytes();
        header.extend_from_slice(&mask);
        let mut masked = Vec::with_capacity(len);
        for (i, &b) in payload.iter().enumerate() {
            masked.push(b ^ mask[i & 3]);
        }
        self.stream.write_all(&header).map_err(|e| format!("CDP write failed: {e}"))?;
        self.stream.write_all(&masked).map_err(|e| format!("CDP write failed: {e}"))?;
        Ok(())
    }

    fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.write_frame(0x1, text.as_bytes())
    }

    fn read_exact_buf(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buf.len() - self.pos < n {
            if self.pos > 0 {
                self.buf.drain(..self.pos);
                self.pos = 0;
            }
            let mut tmp = [0u8; 8192];
            let got = match self.stream.read(&mut tmp) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("CDP read failed: {e}")),
            };
            if got == 0 {
                return Err("CDP WebSocket connection closed".into());
            }
            self.buf.extend_from_slice(&tmp[..got]);
        }
        let out = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        Ok(out)
    }

    fn recv_text(&mut self) -> Result<String, String> {
        let mut message = Vec::new();
        let mut in_fragment = false;
        loop {
            let head = self.read_exact_buf(2)?;
            let fin = (head[0] & 0x80) != 0;
            let opcode = head[0] & 0x0F;
            let masked = (head[1] & 0x80) != 0;
            let len_byte = head[1] & 0x7F;
            let payload_len: usize = match len_byte {
                0..=125 => len_byte as usize,
                126 => {
                    let ext = self.read_exact_buf(2)?;
                    u16::from_be_bytes([ext[0], ext[1]]) as usize
                }
                _ => {
                    let ext = self.read_exact_buf(8)?;
                    usize::try_from(u64::from_be_bytes([ext[0], ext[1], ext[2], ext[3], ext[4], ext[5], ext[6], ext[7]]))
                        .map_err(|_| "CDP WebSocket frame length overflow")?
                }
            };
            if payload_len > MAX_WS_BYTES || message.len().saturating_add(payload_len) > MAX_WS_BYTES {
                return Err("CDP WebSocket message exceeded 32 MiB limit".into());
            }
            let mask_key = if masked {
                let m = self.read_exact_buf(4)?;
                Some([m[0], m[1], m[2], m[3]])
            } else {
                None
            };
            let mut payload = self.read_exact_buf(payload_len)?;
            if let Some(m) = mask_key {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= m[i & 3];
                }
            }
            match opcode {
                0x8 => return Err("CDP target closed WebSocket session".into()),
                0x9 => {
                    let _ = self.write_frame(0xA, &payload);
                }
                0xA => {}
                0x1 | 0x2 => {
                    message.clear();
                    message.extend_from_slice(&payload);
                    if fin {
                        return String::from_utf8(message).map_err(|_| "CDP frame was not valid UTF-8".into());
                    }
                    in_fragment = true;
                }
                0x0 if in_fragment => {
                    message.extend_from_slice(&payload);
                    if fin {
                        return String::from_utf8(message).map_err(|_| "CDP fragmented frame was not valid UTF-8".into());
                    }
                }
                _ => {}
            }
        }
    }
}

pub struct CdpSession {
    ws_url: String,
    ws: WsClient,
    next_id: u64,
}

impl CdpSession {
    pub fn connect(ws_url: &str) -> Result<Self, String> {
        let Endpoint::Ws { host, port, path } = parse_endpoint(ws_url)? else {
            return Err(format!("expected ws:// URL for CDP target, got '{ws_url}'"));
        };
        let ws = WsClient::connect(&host, port, &path)?;
        Ok(CdpSession { ws_url: ws_url.to_string(), ws, next_id: 1 })
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({ "id": id, "method": method, "params": params }).to_string();
        self.ws.send_text(&payload)?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        while Instant::now() < deadline {
            let text = self.ws.recv_text()?;
            let msg: Value = serde_json::from_str(&text).map_err(|e| format!("invalid JSON from CDP: {e}"))?;
            if msg.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let message = err.get("message").and_then(Value::as_str).unwrap_or("CDP call failed");
                return Err(format!("CDP {method}: {message}"));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(format!("CDP {method} timed out"))
    }

    fn eval_json(&mut self, expression: &str) -> Result<Value, String> {
        let res = self.call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": false
            }),
        )?;
        if let Some(exc) = res.get("exceptionDetails") {
            let desc = exc
                .get("exception")
                .and_then(|e| e.get("description").or_else(|| e.get("value")))
                .and_then(Value::as_str)
                .or_else(|| exc.get("text").and_then(Value::as_str))
                .unwrap_or("page script error");
            return Err(format!("DOM script failed: {desc}"));
        }
        Ok(res.get("result").and_then(|r| r.get("value")).cloned().unwrap_or(Value::Null))
    }
}

/// Fetch open CDP targets from an HTTP endpoint (`GET /json/list`) or synthesize a single target
/// from a direct `ws://` endpoint.
pub fn fetch_targets(endpoint: &Endpoint, timeout: Duration) -> Result<Vec<TargetInfo>, String> {
    match endpoint {
        Endpoint::Ws { .. } => Ok(vec![TargetInfo {
            id: "direct".into(),
            title: String::new(),
            url: String::new(),
            kind: "page".into(),
            ws_url: endpoint.display(),
        }]),
        Endpoint::Http { host, port } => {
            let body = http_get(host, *port, "/json/list", timeout)?;
            parse_targets_json(&body)
        }
    }
}

pub fn parse_targets_json(body: &str) -> Result<Vec<TargetInfo>, String> {
    let val: Value = serde_json::from_str(body).map_err(|e| format!("invalid /json/list from CDP: {e}"))?;
    let arr = val.as_array().ok_or("CDP /json/list did not return an array")?;
    let mut out = Vec::new();
    for item in arr {
        let ws_url = item.get("webSocketDebuggerUrl").and_then(Value::as_str).unwrap_or("").to_string();
        if ws_url.is_empty() {
            continue;
        }
        // Only accept loopback WebSocket URLs returned by /json/list.
        if !matches!(parse_endpoint(&ws_url), Ok(Endpoint::Ws { .. })) {
            continue;
        }
        out.push(TargetInfo {
            id: item.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            title: item.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
            url: item.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
            kind: item.get("type").and_then(Value::as_str).unwrap_or("").to_string(),
            ws_url,
        });
    }
    Ok(out)
}

/// Read port from a Chromium `DevToolsActivePort` file (line 1 = port, line 2 = browser ws path).
pub fn parse_devtools_active_port(content: &str) -> Option<Endpoint> {
    let port: u16 = content.lines().next()?.trim().parse().ok()?;
    (port != 0).then(|| Endpoint::Http { host: "127.0.0.1".into(), port })
}

fn candidate_devtools_port_files() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    // deskcrew's own launched profiles
    let own = shot::home().join("cdp-profiles");
    if let Ok(entries) = fs::read_dir(&own) {
        for entry in entries.flatten() {
            paths.push(entry.path().join("DevToolsActivePort"));
        }
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        for rel in [
            r"Google\Chrome\User Data\DevToolsActivePort",
            r"Microsoft\Edge\User Data\DevToolsActivePort",
            r"BraveSoftware\Brave-Browser\User Data\DevToolsActivePort",
            r"Vivaldi\User Data\DevToolsActivePort",
            r"Chromium\User Data\DevToolsActivePort",
        ] {
            paths.push(local.join(rel));
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let support = home.join("Library").join("Application Support");
        for rel in [
            "Google/Chrome/DevToolsActivePort",
            "Microsoft Edge/DevToolsActivePort",
            "BraveSoftware/Brave-Browser/DevToolsActivePort",
            "Vivaldi/DevToolsActivePort",
            "Chromium/DevToolsActivePort",
        ] {
            paths.push(support.join(rel));
        }
    }
    #[cfg(target_os = "linux")]
    {
        let cfg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        if let Some(cfg) = cfg {
            for rel in [
                "google-chrome/DevToolsActivePort",
                "chromium/DevToolsActivePort",
                "microsoft-edge/DevToolsActivePort",
                "BraveSoftware/Brave-Browser/DevToolsActivePort",
                "vivaldi/DevToolsActivePort",
            ] {
                paths.push(cfg.join(rel));
            }
        }
    }
    paths
}

// ---------------------------------------------------------------------------
// DOM Node Model, Stable Refs, Snapshot Walker JS, and Actions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DomNode {
    pub dom_id: u64,
    pub identity: String,
    pub parts: Parts,
    pub rect: (i32, i32, i32, i32),
    pub is_password: bool,
    pub disabled: bool,
    pub readonly: bool,
}

#[derive(Default)]
pub struct DomRefs {
    next: u32,
    by_id: HashMap<String, Vec<u32>>,
}

impl DomRefs {
    pub fn assign(&mut self, nodes: Vec<DomNode>) -> BTreeMap<u32, DomNode> {
        let mut used: HashMap<String, usize> = HashMap::new();
        let mut out = BTreeMap::new();
        for node in nodes {
            let slot = used.entry(node.identity.clone()).or_insert(0);
            let idx = *slot;
            *slot += 1;
            let ids = self.by_id.entry(node.identity.clone()).or_default();
            let id = if idx < ids.len() {
                ids[idx]
            } else {
                self.next += 1;
                ids.push(self.next);
                self.next
            };
            out.insert(id, node);
        }
        out
    }
}

/// DOM walker script executed in one `Runtime.evaluate` round-trip. Pierces open Shadow DOM and
/// same-origin iframes, computes accessible names and live CSS viewport rects, redacts password
/// values, and keeps WeakMap element handles in `window.__deskcrew` without mutating page DOM.
const DOM_SNAPSHOT_JS: &str = r#"(() => {
  const MAX_NODES = 1500;
  const INCLUDE_ALL = __ALL__;
  const store = (window.__deskcrew = window.__deskcrew || { next: 1, byId: new Map(), idOf: new WeakMap() });
  for (const [k, v] of store.byId) {
    if (!v || !v.isConnected) store.byId.delete(k);
  }
  const getId = (el) => {
    let id = store.idOf.get(el);
    if (!id) {
      id = store.next++;
      store.idOf.set(el, id);
    }
    store.byId.set(id, el);
    return id;
  };
  const clip = (s, n) => {
    if (!s) return '';
    const flat = String(s).replace(/\s+/g, ' ').trim();
    return flat.length <= n ? flat : flat.slice(0, n) + '...';
  };
  const vw = window.innerWidth || document.documentElement.clientWidth || 1280;
  const vh = window.innerHeight || document.documentElement.clientHeight || 800;

  const isHidden = (el, style) => {
    if (el.hidden || el.getAttribute('aria-hidden') === 'true') return true;
    if (!style) return true;
    if (style.display === 'none' || style.visibility === 'hidden' || style.visibility === 'collapse' || style.contentVisibility === 'hidden') return true;
    if (parseFloat(style.opacity) === 0 && el.tagName !== 'INPUT') return true;
    return false;
  };

  const labelText = (el, root) => {
    const labelledBy = el.getAttribute('aria-labelledby');
    if (labelledBy) {
      const parts = labelledBy.trim().split(/\s+/).map(id => {
        const ref = (root && root.getElementById ? root.getElementById(id) : null) || document.getElementById(id);
        return ref ? (ref.innerText || ref.textContent || '') : '';
      }).filter(Boolean);
      if (parts.length) return clip(parts.join(' '), 120);
    }
    const aria = el.getAttribute('aria-label');
    if (aria && aria.trim()) return clip(aria, 120);
    if (el.labels && el.labels.length) {
      const txt = Array.from(el.labels).map(l => l.innerText || l.textContent || '').join(' ');
      if (txt.trim()) return clip(txt, 120);
    }
    if (el.id && root && root.querySelector) {
      try {
        const lab = root.querySelector('label[for="' + CSS.escape(el.id) + '"]');
        if (lab) {
          const txt = lab.innerText || lab.textContent || '';
          if (txt.trim()) return clip(txt, 120);
        }
      } catch (_) {}
    }
    const alt = el.getAttribute('alt');
    if (alt && alt.trim()) return clip(alt, 120);
    const placeholder = el.getAttribute('placeholder');
    if (placeholder && placeholder.trim()) return clip(placeholder, 120);
    const title = el.getAttribute('title');
    if (title && title.trim()) return clip(title, 120);
    const tag = el.tagName;
    if (tag === 'INPUT' && (el.type === 'button' || el.type === 'submit' || el.type === 'reset')) {
      return clip(el.value || '', 120);
    }
    if (tag !== 'INPUT' && tag !== 'TEXTAREA' && tag !== 'SELECT') {
      const txt = el.innerText || el.textContent || '';
      if (txt.trim()) return clip(txt, 120);
    }
    return clip(el.getAttribute('name') || '', 80);
  };

  const mapRole = (el) => {
    const explicit = (el.getAttribute('role') || '').trim().toLowerCase();
    const tag = el.tagName.toLowerCase();
    if (explicit) {
      switch (explicit) {
        case 'button': return 'button';
        case 'link': return 'link';
        case 'textbox': case 'searchbox': return 'edit';
        case 'checkbox': case 'switch': return 'checkbox';
        case 'radio': return 'radio';
        case 'combobox': case 'listbox': return 'combobox';
        case 'menuitem': case 'menuitemcheckbox': case 'menuitemradio': return 'menuitem';
        case 'tab': return 'tab';
        case 'slider': return 'slider';
        case 'spinbutton': return 'spinner';
        case 'progressbar': return 'progress';
        case 'table': case 'grid': case 'treegrid': return 'table';
        case 'row': return 'row';
        case 'heading': return 'header';
        case 'img': return 'image';
        case 'option': case 'treeitem': return 'item';
        case 'alert': case 'status': return '';
      }
    }
    if (tag === 'button' || tag === 'summary') return 'button';
    if (tag === 'a' && el.hasAttribute('href')) return 'link';
    if (tag === 'textarea') return 'edit';
    if (tag === 'select') return 'combobox';
    if (tag === 'table') return 'table';
    if (tag === 'tr') return 'row';
    if (/^h[1-6]$/.test(tag)) return 'header';
    if (tag === 'canvas') return 'image';
    if (tag === 'img' && (el.getAttribute('alt') || '').trim()) return 'image';
    if (tag === 'input') {
      const t = (el.type || 'text').toLowerCase();
      if (t === 'hidden') return null;
      if (t === 'checkbox') return 'checkbox';
      if (t === 'radio') return 'radio';
      if (t === 'button' || t === 'submit' || t === 'reset' || t === 'image' || t === 'file') return 'button';
      if (t === 'range') return 'slider';
      if (t === 'number') return 'spinner';
      return 'edit';
    }
    if (el.isContentEditable) return 'edit';
    if (el.hasAttribute('onclick') || (el.hasAttribute('tabindex') && parseInt(el.getAttribute('tabindex'), 10) >= 0)) {
      const hasInteractiveChild = el.querySelector && el.querySelector('a[href],button,input,textarea,select,summary,[role="button"],[role="link"],[role="textbox"],[role="checkbox"],[role="radio"],[role="combobox"],[role="tab"],[role="menuitem"]');
      if (!hasInteractiveChild) return 'button';
    }
    return null;
  };

  const identityOf = (el, role, name, path) => {
    const id = el.id ? '#' + el.id : '';
    const testid = el.getAttribute('data-testid') || el.getAttribute('data-test-id') || '';
    const attrName = el.getAttribute('name') || '';
    if (id) return role + '|' + id;
    if (testid) return role + '|tid:' + testid;
    if (attrName) return role + '|n:' + attrName;
    if (name) return role + '|lbl:' + name.slice(0, 40) + '|' + path;
    return role + '|' + path;
  };

  const nodes = [];
  let hidden = 0;

  const pushNode = (el, role, root, ox, oy, path, cells) => {
    if (nodes.length >= MAX_NODES) return;
    const r = el.getBoundingClientRect();
    const x = Math.round(r.left + ox);
    const y = Math.round(r.top + oy);
    const w = Math.round(r.width);
    const h = Math.round(r.height);
    if (w <= 0 || h <= 0) return;
    const offscreen = (x + w <= 0 || y + h <= 0 || x >= vw || y >= vh);
    if (offscreen && !INCLUDE_ALL) {
      hidden++;
      return;
    }
    const tag = el.tagName.toLowerCase();
    const isPassword = tag === 'input' && (el.type || '').toLowerCase() === 'password';
    const disabled = Boolean(el.disabled) || el.getAttribute('aria-disabled') === 'true';
    const readonly = Boolean(el.readOnly) || el.getAttribute('aria-readonly') === 'true';
    const editable = (role === 'edit') || Boolean(el.isContentEditable);
    const checkable = role === 'checkbox' || role === 'radio';
    const selectable = tag === 'select' || role === 'combobox';
    let name = role === 'row' ? '' : labelText(el, root);
    let value = null;
    if (!isPassword) {
      if (tag === 'input' && !checkable && role !== 'button') {
        if (el.value) value = clip(el.value, 120);
      } else if (tag === 'textarea') {
        if (el.value) value = clip(el.value, 120);
      } else if (tag === 'select') {
        const opt = el.options && el.selectedIndex >= 0 ? el.options[el.selectedIndex] : null;
        if (opt && opt.text) value = clip(opt.text, 120);
      } else if (el.isContentEditable) {
        const txt = el.innerText || el.textContent || '';
        if (txt.trim()) value = clip(txt, 120);
        if (name === value) name = clip(el.getAttribute('aria-label') || el.getAttribute('placeholder') || '', 120);
      }
    }
    const states = [];
    if (checkable) {
      const ariaChecked = el.getAttribute('aria-checked');
      const isOn = ariaChecked !== null ? ariaChecked === 'true' : Boolean(el.checked);
      states.push(isOn ? 'on' : 'off');
    }
    const expanded = el.getAttribute('aria-expanded');
    if (expanded === 'true') states.push('open');
    else if (expanded === 'false') states.push('closed');
    else if (tag === 'summary' && el.parentElement && el.parentElement.tagName === 'DETAILS') {
      states.push(el.parentElement.open ? 'open' : 'closed');
    }
    if (el.getAttribute('aria-selected') === 'true') states.push('selected');
    if (disabled) states.push('disabled');
    if (readonly && editable) states.push('readonly');
    if (!name && value === null && (!cells || !cells.length) && role === '') return;

    nodes.push({
      dom_id: getId(el),
      identity: identityOf(el, role, name, path),
      role,
      name,
      value,
      states,
      cells: cells || [],
      rect: [x, y, w, h],
      tag,
      is_password: isPassword,
      disabled,
      readonly,
      editable,
      checkable,
      selectable
    });
  };

  const SKIP_TAGS = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'HEAD', 'META', 'LINK', 'TITLE', 'BR', 'HR', 'OPTION']);

  const walk = (root, node, ox, oy, path, depth) => {
    if (!node || depth > 40 || nodes.length >= MAX_NODES) return;
    if (node.nodeType !== 1) return;
    const tag = node.tagName;
    if (SKIP_TAGS.has(tag)) return;
    const win = node.ownerDocument && node.ownerDocument.defaultView ? node.ownerDocument.defaultView : window;
    const style = win.getComputedStyle(node);
    if (isHidden(node, style)) return;

    if (tag === 'IFRAME') {
      try {
        const doc = node.contentDocument;
        if (doc && doc.body) {
          const ir = node.getBoundingClientRect();
          walk(doc, doc.body, ox + ir.left, oy + ir.top, path + '>if', depth + 1);
        }
      } catch (_) {}
      return;
    }

    const role = mapRole(node);
    if (role === 'row') {
      const cellEls = Array.from(node.children).filter(c => c.tagName === 'TD' || c.tagName === 'TH' || c.getAttribute('role') === 'cell' || c.getAttribute('role') === 'gridcell' || c.getAttribute('role') === 'columnheader');
      const cells = cellEls.map(c => clip(c.innerText || c.textContent || '', 60)).filter(Boolean);
      if (cells.length) {
        pushNode(node, 'row', root, ox, oy, path, cells);
      }
      // Still walk interactive controls inside table cells.
      for (let i = 0; i < cellEls.length; i++) {
        const kids = cellEls[i].children;
        for (let j = 0; j < kids.length; j++) {
          walk(root, kids[j], ox, oy, path + '>c' + i + '.' + j, depth + 1);
        }
      }
      return;
    }

    if (role !== null) {
      pushNode(node, role, root, ox, oy, path, null);
      // Leaf interactive controls do not need duplicate inner text nodes,
      // but generic containers (<div>/<section>/etc.) with role/tabindex still descend into children.
      const isLeafTag = tag === 'BUTTON' || tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT'
        || tag === 'A' || tag === 'SUMMARY' || tag === 'IMG' || tag === 'CANVAS'
        || /^H[1-6]$/.test(tag) || Boolean(node.isContentEditable);
      if (isLeafTag) {
        if (node.shadowRoot) {
          const sk = node.shadowRoot.children;
          for (let i = 0; i < sk.length; i++) walk(node.shadowRoot, sk[i], ox, oy, path + '>sr' + i, depth + 1);
        }
        return;
      }
    } else {
      // Check direct non-empty leaf text on non-interactive elements (status text, messages, labels).
      let direct = '';
      for (const ch of node.childNodes) {
        if (ch.nodeType === 3 && ch.nodeValue) direct += ch.nodeValue;
      }
      direct = clip(direct, 120);
      if (direct && tag !== 'LABEL') {
        pushNode(node, '', root, ox, oy, path, null);
      }
    }

    if (node.shadowRoot) {
      const sk = node.shadowRoot.children;
      for (let i = 0; i < sk.length; i++) {
        walk(node.shadowRoot, sk[i], ox, oy, path + '>sr' + i, depth + 1);
      }
    }
    const children = node.children;
    if (tag === 'DETAILS' && !node.open) {
      for (let i = 0; i < children.length; i++) {
        if (children[i].tagName === 'SUMMARY') {
          walk(root, children[i], ox, oy, path + '>' + i, depth + 1);
          break;
        }
      }
      return;
    }
    for (let i = 0; i < children.length; i++) {
      walk(root, children[i], ox, oy, path + '>' + i, depth + 1);
    }
  };

  if (document.body) {
    walk(document, document.body, 0, 0, 'b', 0);
  }
  return {
    title: document.title || '',
    url: location.href || '',
    viewport: [vw, vh],
    hidden,
    nodes
  };
})()"#;

struct DomSnapshot {
    title: String,
    url: String,
    viewport: (i32, i32),
    hidden: usize,
    nodes: Vec<DomNode>,
}

fn parse_dom_snapshot(val: &Value) -> Result<DomSnapshot, String> {
    let title = val.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    let url = val.get("url").and_then(Value::as_str).unwrap_or("").to_string();
    let vp = val.get("viewport").and_then(Value::as_array);
    let viewport = match vp.map(Vec::as_slice) {
        Some([w, h]) => (w.as_i64().unwrap_or(1280) as i32, h.as_i64().unwrap_or(800) as i32),
        _ => (1280, 800),
    };
    let hidden = val.get("hidden").and_then(Value::as_u64).unwrap_or(0) as usize;
    let raw_nodes = val.get("nodes").and_then(Value::as_array).ok_or("CDP DOM snapshot returned no nodes")?;
    let mut nodes = Vec::with_capacity(raw_nodes.len());
    for item in raw_nodes {
        let dom_id = item.get("dom_id").and_then(Value::as_u64).unwrap_or(0);
        if dom_id == 0 {
            continue;
        }
        let role = item.get("role").and_then(Value::as_str).unwrap_or("").to_string();
        let name = item.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let is_password = item.get("is_password").and_then(Value::as_bool).unwrap_or(false);
        let value = if is_password {
            None
        } else {
            item.get("value").and_then(Value::as_str).map(str::to_string)
        };
        let mut states: Vec<&'static str> = Vec::new();
        if let Some(arr) = item.get("states").and_then(Value::as_array) {
            for s in arr.iter().filter_map(Value::as_str) {
                match s {
                    "on" => states.push("on"),
                    "off" => states.push("off"),
                    "open" => states.push("open"),
                    "closed" => states.push("closed"),
                    "selected" => states.push("selected"),
                    "disabled" => states.push("disabled"),
                    "readonly" => states.push("readonly"),
                    _ => {}
                }
            }
        }
        let cells: Vec<String> = item
            .get("cells")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let rect = match item.get("rect").and_then(Value::as_array).map(Vec::as_slice) {
            Some([x, y, w, h]) => (
                x.as_i64().unwrap_or(0) as i32,
                y.as_i64().unwrap_or(0) as i32,
                w.as_i64().unwrap_or(0) as i32,
                h.as_i64().unwrap_or(0) as i32,
            ),
            _ => (0, 0, 0, 0),
        };
        nodes.push(DomNode {
            dom_id,
            identity: item.get("identity").and_then(Value::as_str).unwrap_or("").to_string(),
            parts: Parts { role, name, value, states, cells },
            rect,
            is_password,
            disabled: item.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            readonly: item.get("readonly").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    Ok(DomSnapshot { title, url, viewport, hidden, nodes })
}

// ---------------------------------------------------------------------------
// CDP Modifier / Key Mapping
// ---------------------------------------------------------------------------

/// CDP Input modifier bitmask: Alt=1, Control=2, Meta=4, Shift=8.
pub fn parse_cdp_modifiers(spec: &str) -> Result<u32, String> {
    let mut mask = 0u32;
    for part in spec.split('+').map(str::trim).filter(|s| !s.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "alt" | "option" => mask |= 1,
            "ctrl" | "control" => mask |= 2,
            "meta" | "cmd" | "command" | "win" | "super" => mask |= 4,
            "shift" => mask |= 8,
            other => return Err(format!("unknown modifier '{other}': use shift, ctrl, alt or meta")),
        }
    }
    Ok(mask)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CdpKeySpec {
    pub modifiers: u32,
    pub key: String,
    pub code: String,
    pub vk: u32,
    pub text: Option<String>,
}

pub fn is_risky_cdp_spec(spec: &CdpKeySpec) -> bool {
    let alt = (spec.modifiers & 1) != 0;
    let ctrl = (spec.modifiers & 2) != 0;
    let meta = (spec.modifiers & 4) != 0;
    let shift = (spec.modifiers & 8) != 0;
    (alt && spec.vk == 115) // Alt+F4
        || (meta && (spec.vk == b'Q' as u32 || spec.vk == b'W' as u32)) // Cmd/Win+Q, Cmd/Win+W
        || (ctrl && (spec.vk == b'Q' as u32 || spec.vk == b'W' as u32)) // Ctrl+Q, Ctrl+W
        || (meta && alt && spec.vk == 27) // Cmd+Option+Esc
        || (ctrl && shift && spec.vk == 27) // Ctrl+Shift+Esc
        || (ctrl && alt && spec.vk == 46) // Ctrl+Alt+Del
}

#[cfg(test)]
pub fn is_risky_cdp_chord(spec: &str) -> bool {
    parse_cdp_key(spec).is_ok_and(|parsed| is_risky_cdp_spec(&parsed))
}

pub fn parse_cdp_key(chord: &str) -> Result<CdpKeySpec, String> {
    let parts: Vec<&str> = chord.split('+').map(str::trim).filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return Err("key needs a key name".into());
    }
    let mut modifiers = 0u32;
    let main = parts.last().copied().unwrap();
    for &mod_part in &parts[..parts.len() - 1] {
        modifiers |= parse_cdp_modifiers(mod_part)?;
    }
    let lower = main.to_ascii_lowercase();
    let (key, code, vk, text) = match lower.as_str() {
        "enter" | "return" => ("Enter".into(), "Enter".into(), 13, Some("\r".into())),
        "tab" => ("Tab".into(), "Tab".into(), 9, None),
        "esc" | "escape" => ("Escape".into(), "Escape".into(), 27, None),
        "backspace" | "back" => ("Backspace".into(), "Backspace".into(), 8, None),
        "delete" | "del" => ("Delete".into(), "Delete".into(), 46, None),
        "space" => (" ".into(), "Space".into(), 32, Some(" ".into())),
        "up" => ("ArrowUp".into(), "ArrowUp".into(), 38, None),
        "down" => ("ArrowDown".into(), "ArrowDown".into(), 40, None),
        "left" => ("ArrowLeft".into(), "ArrowLeft".into(), 37, None),
        "right" => ("ArrowRight".into(), "ArrowRight".into(), 39, None),
        "home" => ("Home".into(), "Home".into(), 36, None),
        "end" => ("End".into(), "End".into(), 35, None),
        "pageup" | "pgup" => ("PageUp".into(), "PageUp".into(), 33, None),
        "pagedown" | "pgdn" => ("PageDown".into(), "PageDown".into(), 34, None),
        "insert" | "ins" => ("Insert".into(), "Insert".into(), 45, None),
        s if s.starts_with('f') && s.len() <= 3 && s[1..].parse::<u32>().is_ok_and(|n| (1..=12).contains(&n)) => {
            let n: u32 = s[1..].parse().unwrap();
            let name = format!("F{n}");
            (name.clone(), name, 111 + n, None)
        }
        s if s.chars().count() == 1 => {
            let ch = s.chars().next().unwrap();
            if ch.is_ascii_alphabetic() {
                let upper = ch.to_ascii_uppercase();
                let key_str = if (modifiers & 8) != 0 { upper.to_string() } else { ch.to_string() };
                let text = if (modifiers & (1 | 2 | 4)) == 0 { Some(key_str.clone()) } else { None };
                (key_str, format!("Key{upper}"), upper as u32, text)
            } else if ch.is_ascii_digit() {
                let text = if (modifiers & (1 | 2 | 4)) == 0 { Some(ch.to_string()) } else { None };
                (ch.to_string(), format!("Digit{ch}"), ch as u32, text)
            } else {
                let text = if (modifiers & (1 | 2 | 4)) == 0 { Some(ch.to_string()) } else { None };
                (ch.to_string(), String::new(), ch as u32, text)
            }
        }
        _ => return Err(format!("unknown key '{main}'")),
    };
    Ok(CdpKeySpec { modifiers, key, code, vk, text })
}

// ---------------------------------------------------------------------------
// PNG Decode -> shot::Frame (reuses existing `png` crate in Cargo.toml)
// ---------------------------------------------------------------------------

pub fn decode_png_to_frame(png_bytes: &[u8]) -> Result<Frame, String> {
    let mut decoder = png::Decoder::new(png_bytes);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("could not decode CDP PNG: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("could not read CDP PNG frame: {e}"))?;
    let data = &buf[..info.buffer_size()];
    let w = info.width;
    let h = info.height;
    let total = (w as usize).checked_mul(h as usize).ok_or("CDP PNG dimensions overflow")?;
    let mut bgra = Vec::with_capacity(total.checked_mul(4).ok_or("CDP PNG size overflow")?);
    match info.color_type {
        png::ColorType::Rgba => {
            for px in data.as_chunks::<4>().0.iter().take(total) {
                bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        png::ColorType::Rgb => {
            for px in data.as_chunks::<3>().0.iter().take(total) {
                bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for px in data.as_chunks::<2>().0.iter().take(total) {
                bgra.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
        }
        png::ColorType::Grayscale => {
            for &g in data.iter().take(total) {
                bgra.extend_from_slice(&[g, g, g, 255]);
            }
        }
        other => return Err(format!("unsupported CDP PNG color type: {other:?}")),
    }
    Ok(Frame { w, h, pixels: bgra })
}

// ---------------------------------------------------------------------------
// Per-Target State & CdpHub (shared across Windows, macOS, and Linux services)
// ---------------------------------------------------------------------------

struct LastCdpShot {
    hash: u64,
    frame: Frame,
    frames: Frames,
}

struct PageState {
    slot: u32,
    refs: DomRefs,
    nodes: BTreeMap<u32, DomNode>,
    lines: Seen,
    hidden: usize,
    title: String,
    url: String,
    viewport: (i32, i32),
    has_viewport: bool,
    seen_by: HashMap<String, Seen>,
    shots_by: HashMap<String, LastCdpShot>,
    session: Option<CdpSession>,
}

impl PageState {
    fn new(slot: u32) -> Self {
        Self {
            slot,
            refs: DomRefs::default(),
            nodes: BTreeMap::new(),
            lines: Seen::new(),
            hidden: 0,
            title: String::new(),
            url: String::new(),
            viewport: (1280, 800),
            has_viewport: false,
            seen_by: HashMap::new(),
            shots_by: HashMap::new(),
            session: None,
        }
    }

    fn ensure_session(&mut self, ws_url: &str) -> Result<&mut CdpSession, String> {
        let needs_new = self.session.as_ref().is_none_or(|s| s.ws_url != ws_url);
        if needs_new {
            self.session = Some(CdpSession::connect(ws_url)?);
        }
        Ok(self.session.as_mut().unwrap())
    }

    /// Run an idempotent read-only CDP call, reconnecting once if the cached socket was closed.
    fn with_session_read<T>(&mut self, ws_url: &str, mut f: impl FnMut(&mut CdpSession) -> Result<T, String>) -> Result<T, String> {
        let first = {
            let sess = self.ensure_session(ws_url)?;
            f(sess)
        };
        match first {
            Ok(val) => Ok(val),
            Err(err) if err.contains("closed") || err.contains("write failed") => {
                self.session = None;
                let sess = self.ensure_session(ws_url)?;
                f(sess)
            }
            Err(err) => {
                if err.contains("read failed") || err.contains("timed out") {
                    self.session = None;
                }
                Err(err)
            }
        }
    }

    /// Run a side-effecting CDP call at most once (never auto-retrying after sending, so clicks,
    /// typing, and navigations cannot double-execute on slow responses or JS dialogs).
    fn with_session_once<T>(&mut self, ws_url: &str, f: impl FnOnce(&mut CdpSession) -> Result<T, String>) -> Result<T, String> {
        let res = {
            let sess = self.ensure_session(ws_url)?;
            f(sess)
        };
        if let Err(ref err) = res
            && (err.contains("closed") || err.contains("read failed") || err.contains("write failed") || err.contains("timed out"))
        {
            self.session = None;
        }
        res
    }

    fn read_dom(&mut self, ws_url: &str, all: bool) -> Result<(), String> {
        let expr = DOM_SNAPSHOT_JS.replace("__ALL__", if all { "true" } else { "false" });
        let val = self.with_session_read(ws_url, |s| s.eval_json(&expr))?;
        let snap = parse_dom_snapshot(&val)?;
        self.title = snap.title;
        self.url = snap.url;
        self.viewport = snap.viewport;
        self.has_viewport = true;
        self.hidden = snap.hidden;
        self.nodes = self.refs.assign(snap.nodes);
        self.lines = self.nodes.iter().map(|(id, n)| (*id, lines::line(&n.parts))).collect();
        Ok(())
    }

    fn settle_dom(&mut self, ws_url: &str, stop: &AtomicBool) -> Result<(), String> {
        let started = Instant::now();
        sleep_stoppable(SETTLE_FIRST, stop)?;
        self.read_dom(ws_url, false)?;
        let mut last = self.lines.clone();
        while started.elapsed() < SETTLE_MOST {
            sleep_stoppable(SETTLE_STEP, stop)?;
            self.read_dom(ws_url, false)?;
            let same = self.lines == last;
            last = self.lines.clone();
            if same && started.elapsed() >= SETTLE_LEAST {
                break;
            }
        }
        Ok(())
    }
}

const STOPPED_MSG: &str = "stopped: the person pressed the stop keys";

fn check_stop(stop: &AtomicBool) -> Result<(), String> {
    if stop.load(Ordering::Relaxed) {
        Err(STOPPED_MSG.into())
    } else {
        Ok(())
    }
}

fn sleep_stoppable(dur: Duration, stop: &AtomicBool) -> Result<(), String> {
    let deadline = Instant::now() + dur;
    loop {
        check_stop(stop)?;
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        thread::sleep(WAIT_STEP.min(deadline - now));
    }
}

struct HubState {
    explicit: Option<Endpoint>,
    launched_ports: Vec<u16>,
    launched_apps: HashSet<String>,
    active_by_agent: HashMap<String, TargetInfo>,
    next_slot: u32,
    pages: HashMap<String, Arc<Mutex<PageState>>>,
    last_miss: HashMap<String, Instant>,
}

pub struct CdpHub {
    enabled: AtomicBool,
    state: Mutex<HubState>,
}

impl Default for CdpHub {
    fn default() -> Self {
        Self::new()
    }
}

impl CdpHub {
    pub fn new() -> Self {
        let explicit = std::env::var("DESKCREW_CDP")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .and_then(|v| parse_endpoint(&v).ok());
        Self::with_explicit(explicit)
    }

    #[cfg(test)]
    pub fn new_isolated() -> Self {
        Self::with_explicit(None)
    }

    fn with_explicit(explicit: Option<Endpoint>) -> Self {
        CdpHub {
            enabled: AtomicBool::new(true),
            state: Mutex::new(HubState {
                explicit,
                launched_ports: Vec::new(),
                launched_apps: HashSet::new(),
                active_by_agent: HashMap::new(),
                next_slot: 1000,
                pages: HashMap::new(),
                last_miss: HashMap::new(),
            }),
        }
    }

    /// Record a CDP port and app name opened by `deskcrew launch <app> --cdp`.
    pub fn register_port(&self, port: u16, app: &str) {
        let mut st = self.state.lock().unwrap();
        if !st.launched_ports.contains(&port) {
            st.launched_ports.push(port);
        }
        let stem = normalize_app_stem(app);
        if !stem.is_empty() {
            st.launched_apps.insert(stem);
        }
        st.last_miss.clear();
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn active_target(&self, who: &str) -> Option<TargetInfo> {
        if !self.is_enabled() {
            return None;
        }
        self.state.lock().unwrap().active_by_agent.get(who).cloned()
    }

    pub fn set_active(&self, who: &str, target: TargetInfo) {
        self.state.lock().unwrap().active_by_agent.insert(who.to_string(), target);
    }

    pub fn clear_active(&self, who: &str) {
        self.state.lock().unwrap().active_by_agent.remove(who);
    }

    fn candidate_endpoints(&self) -> (Vec<Endpoint>, bool) {
        if !self.is_enabled() {
            return (Vec::new(), false);
        }
        let st = self.state.lock().unwrap();
        if let Some(ep) = &st.explicit {
            return (vec![ep.clone()], true);
        }
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for &port in &st.launched_ports {
            let ep = Endpoint::Http { host: "127.0.0.1".into(), port };
            if seen.insert(ep.display()) {
                out.push(ep);
            }
        }
        drop(st);
        for file in candidate_devtools_port_files() {
            if let Ok(content) = fs::read_to_string(&file)
                && let Some(ep) = parse_devtools_active_port(&content)
                && seen.insert(ep.display())
            {
                out.push(ep);
            }
        }
        let default_ep = Endpoint::Http { host: "127.0.0.1".into(), port: 9222 };
        if seen.insert(default_ep.display()) {
            out.push(default_ep);
        }
        (out, false)
    }

    pub fn discover_targets(&self) -> Vec<(Endpoint, Vec<TargetInfo>)> {
        let (endpoints, explicit) = self.candidate_endpoints();
        let mut found = Vec::new();
        for ep in endpoints {
            let key = ep.display();
            if !explicit {
                let recently_missed = self
                    .state
                    .lock()
                    .unwrap()
                    .last_miss
                    .get(&key)
                    .is_some_and(|at| at.elapsed() < NEGATIVE_CACHE_TTL);
                if recently_missed {
                    continue;
                }
            }
            let timeout = if explicit { CONNECT_TIMEOUT } else { PROBE_TIMEOUT };
            match fetch_targets(&ep, timeout) {
                Ok(targets) if !targets.is_empty() => {
                    self.state.lock().unwrap().last_miss.remove(&key);
                    found.push((ep, targets));
                }
                _ => {
                    self.state.lock().unwrap().last_miss.insert(key, Instant::now());
                }
            }
        }
        found
    }

    /// Match a desktop window (`title`, `app`) to a live CDP page target if CDP is active and
    /// reachable for that browser or CDP-launched Electron window. Non-browser OS windows are
    /// never hijacked even when an explicit CDP endpoint is connected.
    pub fn target_for_window(&self, title: &str, app: &str, is_chromium: bool) -> Option<TargetInfo> {
        if !self.is_enabled() {
            return None;
        }
        let stem = normalize_app_stem(app);
        let (explicit, is_launched) = {
            let st = self.state.lock().unwrap();
            (st.explicit.is_some(), !stem.is_empty() && st.launched_apps.contains(&stem))
        };
        let eligible = is_chromium || is_browser_app(app) || is_launched;
        if !eligible {
            return None;
        }
        let allow_single_fallback = explicit || is_launched;
        for (_, targets) in self.discover_targets() {
            if let Some(hit) = match_page_target(title, &targets, allow_single_fallback) {
                return Some(hit.clone());
            }
        }
        None
    }

    /// Resolve a `cdp`, `cdp:<query>`, or `t<N>` window spec directly to a CDP target (works even
    /// for headless Chromium with no OS window).
    pub fn pick_direct_spec(&self, who: &str, spec: &str) -> Option<Result<TargetInfo, String>> {
        let lower = spec.to_ascii_lowercase();
        if lower == "cdp" {
            if let Some(cur) = self.active_target(who) {
                return Some(Ok(cur));
            }
            let all = self.all_page_targets();
            return Some(all.into_iter().next().ok_or_else(|| "no CDP browser pages found: launch chrome --cdp or run deskcrew cdp 9222".into()));
        }
        if let Some(num) = lower.strip_prefix('t').and_then(|n| n.parse::<usize>().ok())
            && num >= 1
        {
            let all = self.all_page_targets();
            if all.is_empty() {
                return None;
            }
            return Some(
                all.get(num - 1)
                    .cloned()
                    .ok_or_else(|| format!("no CDP tab t{num}: run deskcrew cdp tabs")),
            );
        }
        if let Some(query) = spec.strip_prefix("cdp:").or_else(|| spec.strip_prefix("CDP:")) {
            let query = query.trim();
            let all = self.all_page_targets();
            if all.is_empty() {
                return Some(Err("no CDP browser pages found: launch chrome --cdp or run deskcrew cdp 9222".into()));
            }
            if query.is_empty() {
                return Some(Ok(all[0].clone()));
            }
            let q = query.to_ascii_lowercase();
            let hit = all
                .into_iter()
                .find(|t| t.title.to_ascii_lowercase().contains(&q) || t.url.to_ascii_lowercase().contains(&q) || t.id == query);
            return Some(hit.ok_or_else(|| format!("no CDP tab matches '{query}': run deskcrew cdp tabs")));
        }
        None
    }

    pub fn all_page_targets(&self) -> Vec<TargetInfo> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (_, targets) in self.discover_targets() {
            for t in targets {
                if matches!(t.kind.as_str(), "page" | "webview") && seen.insert(t.ws_url.clone()) {
                    out.push(t);
                }
            }
        }
        out
    }

    /// Handle `deskcrew cdp ...`: status, connect `<port|url>`, `off`, `on`, `tabs`, `use`,
    /// `goto <url>`, `back`, `forward`, `reload`.
    pub fn command(&self, who: &str, args: &[String]) -> Result<String, String> {
        let sub = args.first().map(String::as_str).unwrap_or("status");
        match sub {
            "status" => {
                let enabled = self.is_enabled();
                let (explicit, ports, active) = {
                    let st = self.state.lock().unwrap();
                    (
                        st.explicit.as_ref().map(Endpoint::display),
                        st.launched_ports.clone(),
                        st.active_by_agent.get(who).cloned(),
                    )
                };
                if !enabled {
                    return Ok("cdp: off (run 'deskcrew cdp on' to re-enable DOM control)".into());
                }
                let discovered = self.discover_targets();
                let mut lines_out = Vec::new();
                let ep_note = explicit.unwrap_or_else(|| "auto".into());
                let page_count: usize = discovered
                    .iter()
                    .map(|(_, ts)| ts.iter().filter(|t| matches!(t.kind.as_str(), "page" | "webview")).count())
                    .sum();
                lines_out.push(format!("cdp: on (endpoint: {ep_note}, {page_count} page targets)"));
                if !ports.is_empty() {
                    let list: Vec<String> = ports.iter().map(u16::to_string).collect();
                    lines_out.push(format!("launched ports: {}", list.join(", ")));
                }
                for (ep, targets) in &discovered {
                    let n = targets.iter().filter(|t| matches!(t.kind.as_str(), "page" | "webview")).count();
                    lines_out.push(format!("  {} ({n} pages)", ep.display()));
                }
                if let Some(cur) = active {
                    lines_out.push(format!("active: \"{}\" {}", lines::clip(&cur.title, 50), cur.url));
                }
                Ok(lines_out.join("\n"))
            }
            "off" => {
                self.enabled.store(false, Ordering::Relaxed);
                let mut st = self.state.lock().unwrap();
                st.active_by_agent.clear();
                st.pages.clear();
                Ok("cdp: off (browser windows now use OS accessibility)".into())
            }
            "on" => {
                self.enabled.store(true, Ordering::Relaxed);
                let mut st = self.state.lock().unwrap();
                st.last_miss.clear();
                Ok("cdp: on".into())
            }
            "tabs" => {
                let tabs = self.all_page_targets();
                if tabs.is_empty() {
                    return Ok("no CDP tabs found (start a browser with --remote-debugging-port=9222 or run 'deskcrew launch chrome --cdp')".into());
                }
                let active_ws = self.active_target(who).map(|t| t.ws_url);
                let out: Vec<String> = tabs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let mark = if active_ws.as_deref() == Some(t.ws_url.as_str()) { "*" } else { "" };
                        format!("t{}{mark} \"{}\" {}", i + 1, lines::clip(&t.title, 50), lines::clip(&t.url, 80))
                    })
                    .collect();
                Ok(out.join("\n"))
            }
            "use" => {
                let spec = args.get(1).ok_or("cdp use <t1|title|url>")?;
                let tabs = self.all_page_targets();
                let picked = if let Some(n) = spec.strip_prefix('t').and_then(|s| s.parse::<usize>().ok())
                    && n >= 1
                {
                    tabs.get(n - 1).cloned()
                } else {
                    let q = spec.to_ascii_lowercase();
                    tabs.into_iter().find(|t| t.title.to_ascii_lowercase().contains(&q) || t.url.to_ascii_lowercase().contains(&q))
                }
                .ok_or_else(|| format!("no CDP tab matches '{spec}': run deskcrew cdp tabs"))?;
                let reply = format!("using \"{}\" {}", lines::clip(&picked.title, 50), picked.url);
                self.set_active(who, picked);
                Ok(reply)
            }
            "goto" => {
                let raw_url = args.get(1).ok_or("cdp goto <url>")?;
                let url = raw_url.trim();
                let lower = url.to_ascii_lowercase();
                if !(lower.starts_with("http://")
                    || lower.starts_with("https://")
                    || lower.starts_with("about:")
                    || lower.starts_with("data:")
                    || lower.starts_with("file://"))
                {
                    return Err("cdp goto requires an http://, https://, file://, data:, or about: URL".into());
                }
                let target = self.active_target(who).or_else(|| self.all_page_targets().into_iter().next())
                    .ok_or("no CDP page connected: run 'deskcrew cdp 9222' or 'deskcrew launch chrome --cdp'")?;
                let handle = self.page_handle(&target.ws_url);
                let never_stop = AtomicBool::new(false);
                let (reply, updated) = {
                    let mut page = handle.lock().unwrap();
                    page.with_session_once(&target.ws_url, |s| s.call("Page.navigate", json!({ "url": url })))?;
                    page.settle_dom(&target.ws_url, &never_stop)?;
                    let reply = format!("navigated to \"{}\" ({} controls)", lines::clip(&page.title, 50), page.lines.len());
                    let mut updated = target;
                    updated.title = page.title.clone();
                    updated.url = page.url.clone();
                    (reply, updated)
                };
                self.set_active(who, updated);
                Ok(reply)
            }
            "back" | "forward" | "reload" => {
                let target = self.active_target(who).or_else(|| self.all_page_targets().into_iter().next())
                    .ok_or("no CDP page connected: run 'deskcrew cdp 9222' or 'deskcrew launch chrome --cdp'")?;
                let handle = self.page_handle(&target.ws_url);
                let never_stop = AtomicBool::new(false);
                let mut page = handle.lock().unwrap();
                match sub {
                    "reload" => {
                        page.with_session_once(&target.ws_url, |s| s.call("Page.reload", json!({})))?;
                    }
                    "back" => {
                        page.with_session_once(&target.ws_url, |s| s.eval_json("history.back(); true"))?;
                    }
                    _ => {
                        page.with_session_once(&target.ws_url, |s| s.eval_json("history.forward(); true"))?;
                    }
                }
                page.settle_dom(&target.ws_url, &never_stop)?;
                Ok(format!("ok \"{}\" ({} controls)", lines::clip(&page.title, 50), page.lines.len()))
            }
            other => {
                let ep = parse_endpoint(other)?;
                let targets = fetch_targets(&ep, CONNECT_TIMEOUT)?;
                let pages: Vec<&TargetInfo> = targets.iter().filter(|t| matches!(t.kind.as_str(), "page" | "webview")).collect();
                self.enabled.store(true, Ordering::Relaxed);
                let mut st = self.state.lock().unwrap();
                st.explicit = Some(ep.clone());
                st.last_miss.clear();
                if let Some(first) = pages.first() {
                    st.active_by_agent.insert(who.to_string(), (*first).clone());
                }
                Ok(format!("cdp connected to {} ({} pages)", ep.display(), pages.len()))
            }
        }
    }

    fn page_handle(&self, ws_url: &str) -> Arc<Mutex<PageState>> {
        let mut guard = self.state.lock().unwrap();
        let st = &mut *guard;
        let next = &mut st.next_slot;
        st.pages
            .entry(ws_url.to_string())
            .or_insert_with(|| {
                *next += 1;
                Arc::new(Mutex::new(PageState::new(*next)))
            })
            .clone()
    }

    pub fn see(&self, who: &str, label: &str, target: &TargetInfo, flags: &ActionFlags) -> Result<String, String> {
        let handle = self.page_handle(&target.ws_url);
        let (reply, updated) = {
            let mut guard = handle.lock().unwrap();
            let page = &mut *guard;
            page.read_dom(&target.ws_url, flags.all)?;
            let filtered = flags.find.is_some() || flags.from > 0;
            let seen = page.seen_by.get(who).cloned().filter(|_| !flags.full && !filtered && !flags.all);
            if let Some(prev) = seen {
                let changes = lines::changes(&prev, &page.lines);
                if changes.len() * 2 <= page.lines.len().max(1) {
                    let snapshot = page.lines.clone();
                    page.seen_by.insert(who.to_string(), snapshot);
                    return Ok(if changes.is_empty() { "no change".into() } else { changes.join("\n") });
                }
            }
            if !filtered {
                let snapshot = page.lines.clone();
                page.seen_by.insert(who.to_string(), snapshot);
            }
            let hidden = if page.hidden > 0 { format!(", {} off screen", page.hidden) } else { String::new() };
            let title = if page.title.is_empty() { &target.title } else { &page.title };
            let mut out = vec![format!("{label} \"{}\" [cdp] {} controls{hidden}", lines::clip(title, 50), page.lines.len())];
            out.extend(lines::page(&page.lines, flags.find.as_deref(), flags.from));
            let updated = TargetInfo { title: title.to_string(), url: page.url.clone(), ..target.clone() };
            (out.join("\n"), updated)
        };
        self.set_active(who, updated);
        Ok(reply)
    }

    fn resolve_control(page: &mut PageState, ws_url: &str, word: &str) -> Result<u32, String> {
        if let Some(id) = word.strip_prefix('e').and_then(|n| n.parse::<u32>().ok()) {
            if !page.nodes.contains_key(&id) {
                page.read_dom(ws_url, false)?;
            }
            return if page.nodes.contains_key(&id) { Ok(id) } else { Err(format!("e{id} is gone: see")) };
        }
        let wanted = word.to_ascii_lowercase();
        for attempt in 0..2 {
            if attempt == 1 || page.nodes.is_empty() {
                page.read_dom(ws_url, false)?;
            }
            let exact: Vec<u32> = page
                .nodes
                .iter()
                .filter(|(_, n)| n.parts.name.to_ascii_lowercase() == wanted)
                .map(|(id, _)| *id)
                .collect();
            let mut hits = if !exact.is_empty() {
                exact
            } else {
                page.nodes
                    .iter()
                    .filter(|(_, n)| n.parts.name.to_ascii_lowercase().contains(&wanted))
                    .map(|(id, _)| *id)
                    .collect()
            };
            if hits.len() > 1 {
                let interactive: Vec<u32> = hits.iter().copied().filter(|id| !page.nodes[id].parts.role.is_empty()).collect();
                if !interactive.is_empty() {
                    hits = interactive;
                }
            }
            match hits.as_slice() {
                [] => continue,
                [one] => return Ok(*one),
                many => {
                    let preview: Vec<String> = many.iter().take(6).map(|id| format!("e{id} {}", page.lines[id])).collect();
                    return Err(format!("'{word}' matches {} controls:\n{}", many.len(), preview.join("\n")));
                }
            }
        }
        Err(format!("no control named '{word}': see"))
    }

    fn parse_point(word: &str) -> Option<(i32, i32)> {
        let (x, y) = word.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    }

    fn resolve_point(page: &mut PageState, who: &str, ws_url: &str, word: &str) -> Result<(f64, f64), String> {
        if let Some((x, y)) = Self::parse_point(word) {
            if let Some(last) = page.shots_by.get(who) {
                let (vx, vy) = last.frames.to_screen(x, y);
                return Ok((vx as f64, vy as f64));
            }
            return Ok((x as f64, y as f64));
        }
        let id = Self::resolve_control(page, ws_url, word)?;
        let dom_id = page.nodes[&id].dom_id;
        Self::scroll_and_center(page, ws_url, dom_id)
    }

    fn scroll_and_box(page: &mut PageState, ws_url: &str, dom_id: u64) -> Result<(f64, f64, f64, f64, f64, f64), String> {
        let js = format!(
            r#"(() => {{
                const el = window.__deskcrew && window.__deskcrew.byId.get({dom_id});
                if (!el || !el.isConnected) return null;
                const frames = [];
                let w = el.ownerDocument && el.ownerDocument.defaultView;
                while (w && w !== window) {{
                    const fe = w.frameElement;
                    if (!fe) break;
                    frames.push(fe);
                    w = w.parent;
                }}
                for (let i = frames.length - 1; i >= 0; i--) {{
                    frames[i].scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
                }}
                el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
                let ox = 0, oy = 0;
                for (const fe of frames) {{
                    const fr = fe.getBoundingClientRect();
                    ox += fr.left; oy += fr.top;
                }}
                const r = el.getBoundingClientRect();
                if (r.width <= 0 || r.height <= 0) return {{ hidden: true }};
                const left = r.left + ox, top = r.top + oy;
                return {{ left, top, width: r.width, height: r.height, x: left + r.width / 2, y: top + r.height / 2 }};
            }})()"#
        );
        let val = page.with_session_read(ws_url, |s| s.eval_json(&js))?;
        if val.is_null() {
            return Err("that element is no longer in the DOM: see".into());
        }
        if val.get("hidden").and_then(Value::as_bool).unwrap_or(false) {
            return Err("that element has zero size".into());
        }
        let x = val.get("x").and_then(Value::as_f64).ok_or("could not locate element")?;
        let y = val.get("y").and_then(Value::as_f64).ok_or("could not locate element")?;
        let left = val.get("left").and_then(Value::as_f64).unwrap_or(x);
        let top = val.get("top").and_then(Value::as_f64).unwrap_or(y);
        let width = val.get("width").and_then(Value::as_f64).unwrap_or(0.0);
        let height = val.get("height").and_then(Value::as_f64).unwrap_or(0.0);
        Ok((left, top, width, height, x, y))
    }

    fn scroll_and_center(page: &mut PageState, ws_url: &str, dom_id: u64) -> Result<(f64, f64), String> {
        let (_, _, _, _, x, y) = Self::scroll_and_box(page, ws_url, dom_id)?;
        Ok((x, y))
    }

    pub fn get(&self, target: &TargetInfo, word: &str) -> Result<String, String> {
        let handle = self.page_handle(&target.ws_url);
        let mut page = handle.lock().unwrap();
        let id = Self::resolve_control(&mut page, &target.ws_url, word)?;
        let node = &page.nodes[&id];
        if node.is_password {
            return Err("refused: that field is a password".into());
        }
        let dom_id = node.dom_id;
        let js = format!(
            r#"(() => {{
                const el = window.__deskcrew && window.__deskcrew.byId.get({dom_id});
                if (!el || !el.isConnected) return {{ gone: true }};
                if (el.tagName === 'INPUT' && (el.type || '').toLowerCase() === 'password') return {{ password: true }};
                if ('value' in el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA')) return {{ text: String(el.value || '') }};
                if (el.tagName === 'SELECT') {{
                    const opt = el.options && el.selectedIndex >= 0 ? el.options[el.selectedIndex] : null;
                    return {{ text: opt ? String(opt.text || '') : '' }};
                }}
                return {{ text: String(el.innerText || el.textContent || '') }};
            }})()"#
        );
        let val = page.with_session_read(&target.ws_url, |s| s.eval_json(&js))?;
        if val.get("gone").and_then(Value::as_bool).unwrap_or(false) {
            return Err(format!("e{id} is gone: see"));
        }
        if val.get("password").and_then(Value::as_bool).unwrap_or(false) {
            return Err("refused: that field is a password".into());
        }
        let text = val.get("text").and_then(Value::as_str).unwrap_or("");
        let clipped: String = text.chars().take(MAX_GET_CHARS).collect();
        Ok(clipped)
    }

    pub fn table(&self, target: &TargetInfo, word: &str, from: usize) -> Result<String, String> {
        let handle = self.page_handle(&target.ws_url);
        let mut page = handle.lock().unwrap();
        let id = Self::resolve_control(&mut page, &target.ws_url, word)?;
        let dom_id = page.nodes[&id].dom_id;
        let skip = from.saturating_sub(1);
        let js = format!(
            r#"(() => {{
                const el = window.__deskcrew && window.__deskcrew.byId.get({dom_id});
                if (!el || !el.isConnected) return null;
                const tbl = el.closest('table,[role="table"],[role="grid"]') || el;
                const rows = Array.from(tbl.querySelectorAll('tr,[role="row"]'));
                const headers = [];
                const data = [];
                for (const r of rows) {{
                    const ths = Array.from(r.querySelectorAll(':scope > th,:scope > [role="columnheader"]'));
                    if (ths.length && !headers.length) {{
                        headers.push(...ths.slice(0, {MAX_TABLE_COLUMNS}).map(c => (c.innerText || c.textContent || '').trim()));
                        continue;
                    }}
                    const cells = Array.from(r.querySelectorAll(':scope > td,:scope > th,:scope > [role="cell"],:scope > [role="gridcell"]'));
                    if (cells.length) {{
                        data.push(cells.slice(0, {MAX_TABLE_COLUMNS}).map(c => (c.innerText || c.textContent || '').trim()));
                    }}
                }}
                return {{ total_rows: data.length, total_columns: Math.max(headers.length, ...data.map(r => r.length), 0), headers, rows: data.slice({skip}, {skip} + {MAX_TABLE_ROWS}) }};
            }})()"#
        );
        let val = page.with_session_read(&target.ws_url, |s| s.eval_json(&js))?;
        if val.is_null() {
            return Err("that table is gone: see".into());
        }
        let total_rows = val.get("total_rows").and_then(Value::as_u64).unwrap_or(0) as usize;
        let total_cols = val.get("total_columns").and_then(Value::as_u64).unwrap_or(0) as usize;
        let headers: Vec<String> = val
            .get("headers")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(|s| lines::clip(s, MAX_CELL_CHARS)).collect())
            .unwrap_or_default();
        let rows: Vec<Vec<String>> = val
            .get("rows")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_array)
                    .map(|r| r.iter().filter_map(Value::as_str).map(|s| lines::clip(s, MAX_CELL_CHARS)).collect())
                    .collect()
            })
            .unwrap_or_default();
        let mut out = vec![format!("{total_rows} rows, {total_cols} columns")];
        if !headers.is_empty() {
            out.push(headers.join("\t"));
        }
        let mut size = out.iter().map(String::len).sum::<usize>();
        let mut shown = 0;
        for row in &rows {
            let line = row.join("\t");
            if size + line.len() > TABLE_CHARS && shown > 0 {
                break;
            }
            size += line.len() + 1;
            out.push(line);
            shown += 1;
        }
        if skip + shown < total_rows {
            out.push(format!("... {} more: table {word} --from {}", total_rows - skip - shown, skip + shown + 1));
        }
        Ok(out.join("\n"))
    }

    pub fn is_state(&self, target: &TargetInfo, word: &str, wanted: &str) -> Result<String, String> {
        let handle = self.page_handle(&target.ws_url);
        let mut page = handle.lock().unwrap();
        let id = Self::resolve_control(&mut page, &target.ws_url, word)?;
        page.read_dom(&target.ws_url, false)?;
        let node = page.nodes.get(&id).ok_or_else(|| format!("e{id} is gone: see"))?;
        let states = &node.parts.states;
        let matches = match wanted.to_ascii_lowercase().as_str() {
            "on" => states.contains(&"on"),
            "off" => states.contains(&"off"),
            "open" => states.contains(&"open"),
            "closed" => states.contains(&"closed"),
            "selected" => states.contains(&"selected"),
            "enabled" => !node.disabled && !states.contains(&"disabled"),
            "disabled" => node.disabled || states.contains(&"disabled"),
            other => return Err(format!("unknown state '{other}': use on|off|open|closed|selected|enabled|disabled")),
        };
        Ok(if matches { "yes" } else { "no" }.into())
    }

    pub fn shot(&self, who: &str, target: &TargetInfo, control: Option<&str>, flags: &ActionFlags) -> Result<String, String> {
        let handle = self.page_handle(&target.ws_url);
        let mut page = handle.lock().unwrap();
        if !page.has_viewport
            && let Ok(vp) = page.with_session_read(&target.ws_url, |s| {
                s.eval_json("[window.innerWidth || document.documentElement.clientWidth || 1280, window.innerHeight || document.documentElement.clientHeight || 800]")
            })
            && let Some([w, h]) = vp.as_array().map(Vec::as_slice)
        {
            let vw = w.as_i64().unwrap_or(1280) as i32;
            let vh = h.as_i64().unwrap_or(800) as i32;
            if vw > 0 && vh > 0 {
                page.viewport = (vw, vh);
                page.has_viewport = true;
            }
        }
        let mut crop_rect = flags.region;
        if let Some(ctrl) = control {
            let id = Self::resolve_control(&mut page, &target.ws_url, ctrl)?;
            let dom_id = page.nodes[&id].dom_id;
            let (left, top, w, h, _, _) = Self::scroll_and_box(&mut page, &target.ws_url, dom_id)?;
            crop_rect = if w > 0.0 && h > 0.0 {
                Some((left.round() as i32, top.round() as i32, w.round().max(1.0) as i32, h.round().max(1.0) as i32))
            } else {
                Some(page.nodes[&id].rect)
            };
        }
        let res = page.with_session_read(&target.ws_url, |s| s.call("Page.captureScreenshot", json!({ "format": "png" })))?;
        let b64 = res.get("data").and_then(Value::as_str).ok_or("CDP Page.captureScreenshot returned no image data")?;
        let png_bytes = base64_decode(b64)?;
        let full = decode_png_to_frame(&png_bytes)?;
        // Ratio between CSS viewport units and captured PNG pixels (handles devicePixelRatio > 1).
        let (vw, vh) = page.viewport;
        let css_per_px = if full.w > 0 && vw > 0 { vw as f32 / full.w as f32 } else { 1.0 };
        let (base_frame, mut origin) = if let Some((rx, ry, rw, rh)) = crop_rect {
            let x0 = rx.max(0);
            let y0 = ry.max(0);
            let x1 = rx.saturating_add(rw).min(vw.max(1));
            let y1 = ry.saturating_add(rh).min(vh.max(1));
            let cw = (x1 - x0).max(1);
            let ch = (y1 - y0).max(1);
            let to_px = |v: i32| ((v.max(0) as f32) / css_per_px).round() as u32;
            let px = to_px(x0).min(full.w.saturating_sub(1));
            let py = to_px(y0).min(full.h.saturating_sub(1));
            let pw = to_px(cw).max(1).min(full.w.saturating_sub(px));
            let ph = to_px(ch).max(1).min(full.h.saturating_sub(py));
            (full.crop(px, py, pw, ph), (x0, y0))
        } else {
            (full, (0, 0))
        };
        let hash = base_frame.hash();
        let is_full_view = control.is_none() && flags.region.is_none();
        if is_full_view
            && !flags.full
            && !flags.marks
            && !flags.rulers
            && !flags.changed
            && let Some(prev) = page.shots_by.get(who)
            && prev.hash == hash
        {
            return Ok("unchanged".into());
        }
        let changed_crop = if flags.changed && is_full_view {
            page.shots_by.get(who).and_then(|prev| base_frame.changed_from(&prev.frame))
        } else {
            None
        };
        let picture = match changed_crop {
            Some((cx, cy, cw, ch)) => {
                origin = (
                    origin.0 + (cx as f32 * css_per_px).round() as i32,
                    origin.1 + (cy as f32 * css_per_px).round() as i32,
                );
                base_frame.crop(cx, cy, cw, ch)
            }
            None => base_frame.crop(0, 0, base_frame.w, base_frame.h),
        };
        let edge = flags.edge.unwrap_or(shot::DEFAULT_EDGE);
        let (mut small, shrink_scale) = picture.shrink(edge);
        let total_scale = css_per_px * shrink_scale;
        if flags.marks {
            if page.nodes.is_empty() {
                let _ = page.read_dom(&target.ws_url, false);
            }
            for (id, node) in &page.nodes {
                if node.parts.role.is_empty() {
                    continue;
                }
                let (nx, ny, nw, nh) = node.rect;
                let mx = ((nx - origin.0) as f32 / total_scale).round() as i32;
                let my = ((ny - origin.1) as f32 / total_scale).round() as i32;
                let mw = (nw as f32 / total_scale).round().max(1.0) as i32;
                let mh = (nh as f32 / total_scale).round().max(1.0) as i32;
                small.mark(mx, my, mw, mh, *id);
            }
        }
        if flags.rulers {
            small.rulers();
        }
        let path = shot::path_for(who, page.slot);
        small.save(&path)?;
        let frames = Frames { x: origin.0, y: origin.1, scale: total_scale };
        if is_full_view {
            page.shots_by.insert(who.to_string(), LastCdpShot { hash, frame: base_frame, frames });
        } else if let Some(prev) = page.shots_by.get_mut(who) {
            prev.frames = frames;
        } else {
            page.shots_by.insert(
                who.to_string(),
                LastCdpShot { hash: 0, frame: Frame { w: 0, h: 0, pixels: Vec::new() }, frames },
            );
        }
        let part = if changed_crop.is_some() { " changed part" } else { "" };
        Ok(format!("{} {}x{} scale {total_scale:.2}{part} [cdp]", path.display(), small.w, small.h))
    }

    pub fn dry_run(&self, label: &str, target: &TargetInfo, steps: &[ActionStep]) -> Result<String, String> {
        let handle = self.page_handle(&target.ws_url);
        let mut page = handle.lock().unwrap();
        if page.nodes.is_empty() {
            page.read_dom(&target.ws_url, false)?;
        }
        let describe = |word: &str| {
            word.strip_prefix('e')
                .and_then(|n| n.parse::<u32>().ok())
                .and_then(|id| page.lines.get(&id))
                .map(|line| format!("{word} ({line})"))
                .unwrap_or_else(|| word.to_string())
        };
        let out: Vec<String> = steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let named: Vec<String> = s.words.iter().map(|w| describe(w)).collect();
                format!("{}. {}", i + 1, named.join(" "))
            })
            .chain(Some(format!("nothing done in {label} [cdp]")))
            .collect();
        Ok(out.join("\n"))
    }

    pub fn run_steps(&self, who: &str, target: &TargetInfo, outer: &ActionFlags, steps: &[ActionStep], stop: &AtomicBool) -> Result<String, String> {
        check_stop(stop)?;
        let handle = self.page_handle(&target.ws_url);
        let mut page = handle.lock().unwrap();
        if page.nodes.is_empty() {
            page.read_dom(&target.ws_url, false)?;
        }
        let before = page.lines.clone();
        let mut notes: Vec<String> = Vec::new();
        for (idx, step) in steps.iter().enumerate() {
            check_stop(stop)?;
            if idx > 0 {
                sleep_stoppable(BETWEEN_STEPS, stop)?;
            }
            let merged = merge_flags(outer, &step.flags);
            let Some((verb, rest)) = step.words.split_first() else { continue };
            match self_step(&mut page, who, &target.ws_url, verb, &merged, rest, stop) {
                Ok(Some(note)) => notes.push(note),
                Ok(None) => {}
                Err(err) => return Err(if steps.len() > 1 { format!("step {}: {err}", idx + 1) } else { err }),
            }
            if let Some(ref wanted) = step.flags.expect {
                let expect_flags = ActionFlags { gone: false, ms: merged.ms, ..ActionFlags::default() };
                if let Err(err) = self_step(&mut page, who, &target.ws_url, "expect", &expect_flags, std::slice::from_ref(wanted), stop) {
                    return Err(if steps.len() > 1 { format!("step {}: {err}", idx + 1) } else { err });
                }
            }
        }
        page.settle_dom(&target.ws_url, stop)?;
        let (changes, complete) = lines::change_report(&before, &page.lines);
        if complete {
            let snapshot = page.lines.clone();
            page.seen_by.insert(who.to_string(), snapshot);
        }
        let mut out = Vec::new();
        if steps.len() > 1 {
            out.push(format!("ok {} steps [cdp]", steps.len()));
        } else if notes.is_empty() {
            out.push("ok [cdp]".into());
        }
        out.extend(notes);
        out.extend(changes);
        Ok(out.join("\n"))
    }
}

fn merge_flags(a: &ActionFlags, b: &ActionFlags) -> ActionFlags {
    ActionFlags {
        full: a.full || b.full,
        all: a.all || b.all,
        find: b.find.clone().or_else(|| a.find.clone()),
        from: if b.from > 0 { b.from } else { a.from },
        right: a.right || b.right,
        double: a.double || b.double,
        triple: a.triple || b.triple,
        middle: a.middle || b.middle,
        back: a.back || b.back,
        forward: a.forward || b.forward,
        hold: b.hold.or(a.hold),
        with: b.with.clone().or_else(|| a.with.clone()),
        button: b.button.clone().or_else(|| a.button.clone()),
        smooth: a.smooth || b.smooth,
        fill: a.fill || b.fill,
        close: a.close || b.close,
        gone: a.gone || b.gone,
        ms: b.ms.or(a.ms),
        expect: b.expect.clone(),
        force: a.force || b.force,
        marks: a.marks || b.marks,
        rulers: a.rulers || b.rulers,
        changed: a.changed || b.changed,
        edge: b.edge.or(a.edge),
        region: b.region.or(a.region),
    }
}

fn cdp_button_name(flags: &ActionFlags) -> Result<(&'static str, u32), String> {
    if let Some(ref b) = flags.button {
        return match b.to_ascii_lowercase().as_str() {
            "left" => Ok(("left", 1)),
            "right" => Ok(("right", 2)),
            "middle" => Ok(("middle", 4)),
            "back" => Ok(("back", 8)),
            "forward" => Ok(("forward", 16)),
            other => Err(format!("unknown button '{other}': use left, right, middle, back or forward")),
        };
    }
    if flags.right {
        Ok(("right", 2))
    } else if flags.middle {
        Ok(("middle", 4))
    } else if flags.back {
        Ok(("back", 8))
    } else if flags.forward {
        Ok(("forward", 16))
    } else {
        Ok(("left", 1))
    }
}

fn dispatch_mouse_click(page: &mut PageState, ws_url: &str, x: f64, y: f64, flags: &ActionFlags, stop: &AtomicBool) -> Result<(), String> {
    let (button, buttons) = cdp_button_name(flags)?;
    let modifiers = match &flags.with {
        Some(spec) => parse_cdp_modifiers(spec)?,
        None => 0,
    };
    let clicks = if flags.triple { 3 } else if flags.double { 2 } else { 1 };
    let hold = flags.hold.map(|ms| Duration::from_millis(ms.min(HOLD_MOST_MS)));
    page.with_session_once(ws_url, |s| {
        s.call("Input.dispatchMouseEvent", json!({ "type": "mouseMoved", "x": x, "y": y, "modifiers": modifiers }))?;
        for count in 1..=clicks {
            check_stop(stop)?;
            s.call(
                "Input.dispatchMouseEvent",
                json!({ "type": "mousePressed", "x": x, "y": y, "button": button, "buttons": buttons, "clickCount": count, "modifiers": modifiers }),
            )?;
            let hold_res = if let Some(d) = hold { sleep_stoppable(d, stop) } else { Ok(()) };
            let rel_res = s.call(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseReleased", "x": x, "y": y, "button": button, "buttons": 0, "clickCount": count, "modifiers": modifiers }),
            );
            hold_res?;
            rel_res?;
        }
        Ok(())
    })
}

fn dispatch_stroke(page: &mut PageState, ws_url: &str, pts: &[(f64, f64)], flags: &ActionFlags, stop: &AtomicBool) -> Result<(), String> {
    if pts.len() < 2 {
        return Err("stroke needs at least two points".into());
    }
    let (button, buttons) = cdp_button_name(flags)?;
    let modifiers = match &flags.with {
        Some(spec) => parse_cdp_modifiers(spec)?,
        None => 0,
    };
    let (sx, sy) = pts[0];
    let mut last = (sx, sy);
    page.with_session_once(ws_url, |s| {
        s.call("Input.dispatchMouseEvent", json!({ "type": "mouseMoved", "x": sx, "y": sy, "modifiers": modifiers }))?;
        s.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mousePressed", "x": sx, "y": sy, "button": button, "buttons": buttons, "clickCount": 1, "modifiers": modifiers }),
        )?;
        let mut stopped: Option<String> = None;
        for &(x, y) in &pts[1..] {
            if let Err(err) = check_stop(stop) {
                stopped = Some(err);
                break;
            }
            s.call(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": x, "y": y, "button": button, "buttons": buttons, "modifiers": modifiers }),
            )?;
            last = (x, y);
        }
        let rel = s.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseReleased", "x": last.0, "y": last.1, "button": button, "buttons": 0, "clickCount": 1, "modifiers": modifiers }),
        );
        if let Some(err) = stopped {
            let _ = rel;
            return Err(err);
        }
        rel?;
        Ok(())
    })
}

fn self_step(page: &mut PageState, who: &str, ws_url: &str, verb: &str, flags: &ActionFlags, args: &[String], stop: &AtomicBool) -> Result<Option<String>, String> {
    check_stop(stop)?;
    match verb {
        "click" => {
            let word = args.first().ok_or("click needs a control or x,y")?;
            if CdpHub::parse_point(word).is_none() {
                let id = CdpHub::resolve_control(page, ws_url, word)?;
                if page.nodes[&id].disabled {
                    return Err("that control is disabled".into());
                }
            }
            let (x, y) = CdpHub::resolve_point(page, who, ws_url, word)?;
            dispatch_mouse_click(page, ws_url, x, y, flags, stop)?;
            Ok(None)
        }
        "hover" => {
            let word = args.first().ok_or("hover needs a control or x,y")?;
            let (x, y) = CdpHub::resolve_point(page, who, ws_url, word)?;
            let modifiers = match &flags.with {
                Some(spec) => parse_cdp_modifiers(spec)?,
                None => 0,
            };
            page.with_session_once(ws_url, |s| {
                s.call("Input.dispatchMouseEvent", json!({ "type": "mouseMoved", "x": x, "y": y, "modifiers": modifiers }))
            })?;
            Ok(None)
        }
        "set" => {
            let [word, value] = args else { return Err("set e3 \"value\"".into()) };
            let id = CdpHub::resolve_control(page, ws_url, word)?;
            let node = &page.nodes[&id];
            if node.disabled {
                return Err("that control is disabled".into());
            }
            if node.readonly {
                return Err("that control is read-only".into());
            }
            let dom_id = node.dom_id;
            let val_json = serde_json::to_string(value).map_err(|e| e.to_string())?;
            let js = format!(
                r#"(() => {{
                    const el = window.__deskcrew && window.__deskcrew.byId.get({dom_id});
                    if (!el || !el.isConnected) return {{ err: 'element is gone: see' }};
                    if (el.disabled || el.getAttribute('aria-disabled') === 'true') return {{ err: 'that control is disabled' }};
                    if (el.readOnly || el.getAttribute('aria-readonly') === 'true') return {{ err: 'that control is read-only' }};
                    el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
                    el.focus();
                    const v = {val_json};
                    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {{
                        const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
                        const desc = Object.getOwnPropertyDescriptor(proto, 'value');
                        if (desc && desc.set) desc.set.call(el, v); else el.value = v;
                        el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                        el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        return {{ ok: true }};
                    }}
                    if (el.isContentEditable) {{
                        el.textContent = v;
                        el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                        return {{ ok: true }};
                    }}
                    return {{ err: 'that control does not take text' }};
                }})()"#
            );
            let res = page.with_session_once(ws_url, |s| s.eval_json(&js))?;
            if let Some(err) = res.get("err").and_then(Value::as_str) {
                return Err(err.to_string());
            }
            Ok(None)
        }
        "clear" => {
            let word = args.first().ok_or("clear needs a control")?;
            self_step(page, who, ws_url, "set", flags, &[word.clone(), String::new()], stop)
        }
        "type" => {
            let (target_word, text) = match args {
                [text] => (None, text.as_str()),
                [word, text] => (Some(word.as_str()), text.as_str()),
                _ => return Err("type [e3] \"text\"".into()),
            };
            if let Some(word) = target_word {
                let id = CdpHub::resolve_control(page, ws_url, word)?;
                let node = &page.nodes[&id];
                if node.disabled {
                    return Err("that control is disabled".into());
                }
                if node.readonly {
                    return Err("that control is read-only".into());
                }
                let dom_id = node.dom_id;
                let js = format!(
                    r#"(() => {{
                        const el = window.__deskcrew && window.__deskcrew.byId.get({dom_id});
                        if (!el || !el.isConnected) return {{ err: 'element is gone: see' }};
                        el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
                        el.focus();
                        if ((el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') && typeof el.setSelectionRange === 'function') {{
                            try {{ const n = el.value.length; el.setSelectionRange(n, n); }} catch (_) {{}}
                        }}
                        return {{ ok: true }};
                    }})()"#
                );
                let res = page.with_session_once(ws_url, |s| s.eval_json(&js))?;
                if let Some(err) = res.get("err").and_then(Value::as_str) {
                    return Err(err.to_string());
                }
            }
            page.with_session_once(ws_url, |s| s.call("Input.insertText", json!({ "text": text })))?;
            Ok(None)
        }
        "check" | "uncheck" | "toggle" => {
            let word = args.first().ok_or(format!("{verb} needs a control"))?;
            let id = CdpHub::resolve_control(page, ws_url, word)?;
            if page.nodes[&id].disabled {
                return Err("that control is disabled".into());
            }
            let is_on = page.nodes[&id].parts.states.contains(&"on");
            if verb == "check" && is_on {
                return Ok(Some("already on".into()));
            }
            if verb == "uncheck" && !is_on {
                return Ok(Some("already off".into()));
            }
            let dom_id = page.nodes[&id].dom_id;
            let (x, y) = CdpHub::scroll_and_center(page, ws_url, dom_id)?;
            dispatch_mouse_click(page, ws_url, x, y, &ActionFlags::default(), stop)?;
            Ok(None)
        }
        "expand" | "collapse" => {
            let word = args.first().ok_or(format!("{verb} needs a control"))?;
            let id = CdpHub::resolve_control(page, ws_url, word)?;
            if page.nodes[&id].disabled {
                return Err("that control is disabled".into());
            }
            let is_open = page.nodes[&id].parts.states.contains(&"open");
            if verb == "expand" && is_open {
                return Ok(Some("already open".into()));
            }
            if verb == "collapse" && !is_open {
                return Ok(Some("already closed".into()));
            }
            let dom_id = page.nodes[&id].dom_id;
            let (x, y) = CdpHub::scroll_and_center(page, ws_url, dom_id)?;
            dispatch_mouse_click(page, ws_url, x, y, &ActionFlags::default(), stop)?;
            Ok(None)
        }
        "select" => {
            let [word, item] = args else { return Err("select e3 \"Item\"".into()) };
            let id = CdpHub::resolve_control(page, ws_url, word)?;
            if page.nodes[&id].disabled {
                return Err("that control is disabled".into());
            }
            let dom_id = page.nodes[&id].dom_id;
            let item_json = serde_json::to_string(item).map_err(|e| e.to_string())?;
            let js = format!(
                r#"(() => {{
                    const el = window.__deskcrew && window.__deskcrew.byId.get({dom_id});
                    if (!el || !el.isConnected) return {{ err: 'element is gone: see' }};
                    if (el.disabled || el.getAttribute('aria-disabled') === 'true') return {{ err: 'that control is disabled' }};
                    const wanted = ({item_json}).toLowerCase().trim();
                    if (el.tagName === 'SELECT') {{
                        const opts = Array.from(el.options || []);
                        let idx = opts.findIndex(o => (o.text || '').toLowerCase().trim() === wanted || (o.value || '').toLowerCase().trim() === wanted);
                        if (idx < 0) idx = opts.findIndex(o => (o.text || '').toLowerCase().includes(wanted));
                        if (idx < 0) return {{ err: 'no option matching ' + {item_json} }};
                        if (opts[idx].disabled) return {{ err: 'that option is disabled' }};
                        el.selectedIndex = idx;
                        el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                        el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        return {{ ok: true }};
                    }}
                    return {{ custom: true }};
                }})()"#
            );
            let res = page.with_session_once(ws_url, |s| s.eval_json(&js))?;
            if let Some(err) = res.get("err").and_then(Value::as_str) {
                return Err(err.to_string());
            }
            if res.get("custom").and_then(Value::as_bool).unwrap_or(false) {
                // Custom ARIA combobox/listbox: click to open, then locate matching [role=option] / [role=menuitem].
                let (x, y) = CdpHub::scroll_and_center(page, ws_url, dom_id)?;
                dispatch_mouse_click(page, ws_url, x, y, &ActionFlags::default(), stop)?;
                sleep_stoppable(Duration::from_millis(80), stop)?;
                let find_opt_js = format!(
                    r#"(() => {{
                        const wanted = ({item_json}).toLowerCase().trim();
                        const opts = Array.from(document.querySelectorAll('[role="option"],[role="menuitem"],li'));
                        const match = opts.find(o => (o.innerText || o.textContent || '').toLowerCase().trim() === wanted)
                            || opts.find(o => (o.innerText || o.textContent || '').toLowerCase().includes(wanted));
                        if (!match) return null;
                        if (match.getAttribute('aria-disabled') === 'true') return {{ disabled: true }};
                        match.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
                        const r = match.getBoundingClientRect();
                        return {{ x: r.left + r.width / 2, y: r.top + r.height / 2 }};
                    }})()"#
                );
                let opt = page.with_session_read(ws_url, |s| s.eval_json(&find_opt_js))?;
                if opt.is_null() {
                    return Err(format!("no option matching '{item}'"));
                }
                if opt.get("disabled").and_then(Value::as_bool).unwrap_or(false) {
                    return Err("that option is disabled".into());
                }
                let ox = opt.get("x").and_then(Value::as_f64).ok_or("option has no position")?;
                let oy = opt.get("y").and_then(Value::as_f64).ok_or("option has no position")?;
                dispatch_mouse_click(page, ws_url, ox, oy, &ActionFlags::default(), stop)?;
            }
            Ok(None)
        }
        "scroll" => {
            let (target, dir, count) = match args {
                [dir] => (None, dir.as_str(), 3i32),
                [a, b] if b.parse::<i32>().is_ok() => (None, a.as_str(), b.parse::<i32>().unwrap().clamp(1, 50)),
                [word, dir] => (Some(word.as_str()), dir.as_str(), 3i32),
                [word, dir, n] => (Some(word.as_str()), dir.as_str(), n.parse::<i32>().unwrap_or(3).clamp(1, 50)),
                _ => return Err("scroll [e3] up|down|left|right [n]".into()),
            };
            let (x, y) = if let Some(word) = target {
                CdpHub::resolve_point(page, who, ws_url, word)?
            } else {
                (page.viewport.0 as f64 / 2.0, page.viewport.1 as f64 / 2.0)
            };
            let delta = (count as f64) * 120.0;
            let (dx, dy) = match dir.to_ascii_lowercase().as_str() {
                "down" => (0.0, delta),
                "up" => (0.0, -delta),
                "right" => (delta, 0.0),
                "left" => (-delta, 0.0),
                other => return Err(format!("unknown scroll direction '{other}': use up|down|left|right")),
            };
            page.with_session_once(ws_url, |s| {
                s.call(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy }),
                )
            })?;
            Ok(None)
        }
        "show" => {
            let word = args.first().ok_or("show needs a control")?;
            let id = CdpHub::resolve_control(page, ws_url, word)?;
            let dom_id = page.nodes[&id].dom_id;
            let _ = CdpHub::scroll_and_center(page, ws_url, dom_id)?;
            Ok(None)
        }
        "key" => {
            let chord = args.first().ok_or("key needs a key name")?;
            let spec = parse_cdp_key(chord)?;
            if is_risky_cdp_spec(&spec) && !flags.force {
                return Err(format!("refused: '{chord}' is a risky shortcut (add --force after asking the person)"));
            }
            let hold = flags.hold.map(|ms| Duration::from_millis(ms.min(HOLD_MOST_MS)));
            page.with_session_once(ws_url, |s| {
                s.call(
                    "Input.dispatchKeyEvent",
                    json!({
                        "type": "rawKeyDown",
                        "modifiers": spec.modifiers,
                        "windowsVirtualKeyCode": spec.vk,
                        "key": spec.key,
                        "code": spec.code,
                    }),
                )?;
                if let Some(ref text) = spec.text {
                    s.call(
                        "Input.dispatchKeyEvent",
                        json!({
                            "type": "char",
                            "modifiers": spec.modifiers,
                            "text": text,
                            "unmodifiedText": text,
                        }),
                    )?;
                }
                let hold_res = if let Some(d) = hold { sleep_stoppable(d, stop) } else { Ok(()) };
                let up_res = s.call(
                    "Input.dispatchKeyEvent",
                    json!({
                        "type": "keyUp",
                        "modifiers": spec.modifiers,
                        "windowsVirtualKeyCode": spec.vk,
                        "key": spec.key,
                        "code": spec.code,
                    }),
                );
                hold_res?;
                up_res?;
                Ok(())
            })?;
            Ok(None)
        }
        "drag" => {
            let [a, b] = args else { return Err("drag a b".into()) };
            let (sx, sy) = CdpHub::resolve_point(page, who, ws_url, a)?;
            let (ex, ey) = CdpHub::resolve_point(page, who, ws_url, b)?;
            let steps = 12usize;
            let mut pts = Vec::with_capacity(steps + 1);
            for i in 0..=steps {
                let t = i as f64 / steps as f64;
                pts.push((sx + (ex - sx) * t, sy + (ey - sy) * t));
            }
            dispatch_stroke(page, ws_url, &pts, flags, stop)?;
            Ok(None)
        }
        "draw" => {
            let (word, script) = match args {
                [] => return Err("draw [e3] \"x,y x,y; rect x,y w,h; circle x,y r\"".into()),
                [first, rest @ ..] if !rest.is_empty() && motion::parse_point(first).is_err() && !motion::is_shape(first) => {
                    (Some(first.as_str()), rest.join(" "))
                }
                _ => (None, args.join(" ")),
            };
            let (offset, scale) = match word {
                Some(w) => {
                    let id = CdpHub::resolve_control(page, ws_url, w)?;
                    let dom_id = page.nodes[&id].dom_id;
                    let (left, top, _, _, _, _) = CdpHub::scroll_and_box(page, ws_url, dom_id)?;
                    ((left as f32, top as f32), 1.0_f32)
                }
                None => {
                    let frames = page.shots_by.get(who).map(|s| s.frames).unwrap_or(Frames { x: 0, y: 0, scale: 1.0 });
                    ((frames.x as f32, frames.y as f32), frames.scale)
                }
            };
            let mut strokes: Vec<Vec<(f32, f32)>> = Vec::new();
            for chunk in script.split(';').filter(|s| !s.trim().is_empty()) {
                let chunk = chunk.trim();
                if let Some(shape) = motion::parse_shape(chunk)? {
                    let transformed = shape.with_flags(flags.fill, flags.close).transform(offset, scale);
                    strokes.extend(motion::generate_shape(&transformed));
                } else {
                    let mut raw_pts = Vec::new();
                    for tok in chunk.split_whitespace() {
                        let (x, y) = motion::parse_point(tok)?;
                        raw_pts.push(((offset.0 + x * scale).round() as i32, (offset.1 + y * scale).round() as i32));
                    }
                    if !raw_pts.is_empty() {
                        strokes.push(motion::trace(&raw_pts, flags.smooth));
                    }
                }
            }
            if strokes.iter().map(Vec::len).sum::<usize>() == 0 {
                return Err("nothing to draw".into());
            }
            for stroke in strokes {
                check_stop(stop)?;
                let pts: Vec<(f64, f64)> = stroke.into_iter().map(|(x, y)| (x as f64, y as f64)).collect();
                dispatch_stroke(page, ws_url, &pts, flags, stop)?;
            }
            Ok(None)
        }
        "wait" | "expect" => {
            let wanted = args.first().ok_or("wait needs text or milliseconds")?;
            let gone = verb == "wait" && flags.gone;
            if verb == "wait" && !gone
                && let Ok(ms) = wanted.parse::<u64>()
            {
                sleep_stoppable(Duration::from_millis(ms.min(WAIT_MOST_MS)), stop)?;
                return Ok(None);
            }
            let timeout = Duration::from_millis(flags.ms.unwrap_or(WAIT_DEFAULT_MS).min(WAIT_MOST_MS));
            let deadline = Instant::now() + timeout;
            let needle = wanted.to_ascii_lowercase();
            loop {
                check_stop(stop)?;
                page.read_dom(ws_url, false)?;
                let present = page.lines.values().any(|line| line.to_ascii_lowercase().contains(&needle))
                    || page.title.to_ascii_lowercase().contains(&needle);
                if (gone && !present) || (!gone && present) {
                    return Ok(None);
                }
                let now = Instant::now();
                if now >= deadline {
                    return Err(if gone {
                        format!("timed out waiting for '{wanted}' to disappear")
                    } else {
                        format!("timed out waiting for '{wanted}'")
                    });
                }
                sleep_stoppable(WAIT_STEP.min(deadline - now), stop)?;
            }
        }
        other => Err(format!("'{other}' is not supported over CDP")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn sha1_and_ws_accept_match_rfc6455_vector() {
        // RFC 6455 Section 1.3 official test vector.
        assert_eq!(ws_accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn base64_roundtrip_handles_padding_and_rejects_bad_chars() {
        for sample in [b"".as_slice(), b"a", b"ab", b"abc", b"hello world!", &[0, 1, 255, 128, 42]] {
            let enc = base64_encode(sample);
            assert_eq!(base64_decode(&enc).unwrap(), sample);
        }
        assert!(base64_decode("bad$char").is_err());
    }

    #[test]
    fn parse_endpoint_allows_loopback_and_rejects_remote_hosts() {
        assert_eq!(parse_endpoint("9222").unwrap(), Endpoint::Http { host: "127.0.0.1".into(), port: 9222 });
        assert_eq!(parse_endpoint("http://localhost:9333").unwrap(), Endpoint::Http { host: "localhost".into(), port: 9333 });
        assert_eq!(
            parse_endpoint("ws://127.0.0.1:9222/devtools/page/ABC").unwrap(),
            Endpoint::Ws { host: "127.0.0.1".into(), port: 9222, path: "/devtools/page/ABC".into() }
        );
        assert_eq!(parse_endpoint("http://[::1]:9222").unwrap(), Endpoint::Http { host: "::1".into(), port: 9222 });
        assert!(parse_endpoint("http://192.168.1.5:9222").is_err());
        assert!(parse_endpoint("ws://evil.example:9222/devtools/page/1").is_err());
        assert!(parse_endpoint("https://127.0.0.1:9222").is_err());
        assert!(parse_endpoint("0").is_err());
        assert!(parse_endpoint("127.0.0.1:9222\r\nX-Injected: 1").is_err());
    }

    #[test]
    fn strip_browser_suffix_and_match_page_target_handle_chrome_edge_and_zwsp() {
        assert_eq!(strip_browser_suffix("GitHub - Google Chrome"), "GitHub");
        assert_eq!(strip_browser_suffix("Docs and 3 more pages - Microsoft\u{200b} Edge"), "Docs");
        let targets = vec![
            TargetInfo {
                id: "1".into(),
                title: "Settings".into(),
                url: "chrome://settings".into(),
                kind: "page".into(),
                ws_url: "ws://127.0.0.1:9222/devtools/page/1".into(),
            },
            TargetInfo {
                id: "2".into(),
                title: "Example Domain".into(),
                url: "https://example.com".into(),
                kind: "page".into(),
                ws_url: "ws://127.0.0.1:9222/devtools/page/2".into(),
            },
            TargetInfo {
                id: "sw".into(),
                title: "Service Worker".into(),
                url: "https://example.com/sw.js".into(),
                kind: "service_worker".into(),
                ws_url: "ws://127.0.0.1:9222/devtools/page/sw".into(),
            },
        ];
        assert_eq!(match_page_target("Example Domain - Google Chrome", &targets, false).unwrap().id, "2");
        assert_eq!(match_page_target("Example Domain and 2 more pages - Microsoft\u{200b} Edge", &targets, false).unwrap().id, "2");
        assert!(match_page_target("Unrelated App", &targets, false).is_none());
        let single = vec![targets[1].clone()];
        assert_eq!(match_page_target("Dynamic Electron Title", &single, true).unwrap().id, "2");
    }

    #[test]
    fn chromium_cdp_args_and_extract_port_preserve_existing_port() {
        assert_eq!(extract_debugging_port("--remote-debugging-port=9229 --incognito"), Some(9229));
        assert_eq!(extract_debugging_port("--remote-debugging-port 9333"), Some(9333));
        assert_eq!(extract_debugging_port("--incognito"), None);
        let (built, port) = chromium_cdp_args("--incognito", "chrome", 9444);
        assert_eq!(port, 9444);
        assert!(built.contains("--remote-debugging-port=9444"));
        assert!(built.contains("--user-data-dir="));
        assert!(built.ends_with("--incognito"));
        let (kept, port2) = chromium_cdp_args("--remote-debugging-port=9222", "chrome", 9444);
        assert_eq!(port2, 9222);
        assert_eq!(kept, "--remote-debugging-port=9222");
    }

    #[test]
    fn parse_cdp_key_and_risky_chords_work() {
        let enter = parse_cdp_key("enter").unwrap();
        assert_eq!((enter.key.as_str(), enter.vk, enter.modifiers), ("Enter", 13, 0));
        let ctrl_a = parse_cdp_key("ctrl+a").unwrap();
        assert_eq!((ctrl_a.key.as_str(), ctrl_a.code.as_str(), ctrl_a.vk, ctrl_a.modifiers, ctrl_a.text), ("a", "KeyA", b'A' as u32, 2, None));
        let shift_a = parse_cdp_key("shift+a").unwrap();
        assert_eq!(shift_a.text.as_deref(), Some("A"));
        assert!(is_risky_cdp_chord("alt+f4"));
        assert!(is_risky_cdp_chord("cmd+q"));
        assert!(!is_risky_cdp_chord("ctrl+a"));
    }

    #[test]
    fn dom_snapshot_redacts_passwords_and_keeps_stable_refs() {
        let snap1 = json!({
            "title": "Login",
            "url": "https://example.test/login",
            "viewport": [1024, 768],
            "hidden": 2,
            "nodes": [
                { "dom_id": 1, "identity": "edit|#user", "role": "edit", "name": "Username", "value": "ada", "states": [], "cells": [], "rect": [10, 10, 200, 30], "tag": "input", "is_password": false, "disabled": false, "readonly": false, "editable": true, "checkable": false, "selectable": false },
                { "dom_id": 2, "identity": "edit|#pass", "role": "edit", "name": "Password", "value": "supersecret", "states": [], "cells": [], "rect": [10, 50, 200, 30], "tag": "input", "is_password": true, "disabled": false, "readonly": false, "editable": true, "checkable": false, "selectable": false },
                { "dom_id": 3, "identity": "button|#go", "role": "button", "name": "Sign in", "value": null, "states": [], "cells": [], "rect": [10, 90, 80, 30], "tag": "button", "is_password": false, "disabled": false, "readonly": false, "editable": false, "checkable": false, "selectable": false }
            ]
        });
        let snap1 = parse_dom_snapshot(&snap1).unwrap();
        assert_eq!(snap1.hidden, 2);
        assert_eq!(snap1.nodes[1].parts.value, None, "password value must be redacted");
        let mut refs = DomRefs::default();
        let assigned1 = refs.assign(snap1.nodes);
        assert_eq!(assigned1[&1].parts.name, "Username");
        assert_eq!(assigned1[&3].parts.name, "Sign in");

        // Second snapshot drops Username, keeps Sign in: Sign in must retain ref e3!
        let snap2 = json!({
            "title": "Login",
            "url": "https://example.test/login",
            "viewport": [1024, 768],
            "hidden": 0,
            "nodes": [
                { "dom_id": 3, "identity": "button|#go", "role": "button", "name": "Sign in", "value": null, "states": ["disabled"], "cells": [], "rect": [10, 90, 80, 30], "tag": "button", "is_password": false, "disabled": true, "readonly": false, "editable": false, "checkable": false, "selectable": false }
            ]
        });
        let snap2 = parse_dom_snapshot(&snap2).unwrap();
        let assigned2 = refs.assign(snap2.nodes);
        assert!(assigned2.contains_key(&3));
        assert!(assigned2[&3].disabled);
    }

    #[test]
    fn mock_cdp_websocket_server_end_to_end() {
        // Real loopback HTTP + RFC 6455 WebSocket mock server exercising CdpHub end-to-end:
        // /json/list, WebSocket upgrade, ping/pong, interleaved CDP events, fragmented frames,
        // see, diff, click, set, get (including password refusal), table, is, shot PNG decode, goto.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let recorded = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded_srv = recorded.clone();
        let clicked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let clicked_srv = clicked.clone();

        // Build a valid tiny 4x4 PNG for Page.captureScreenshot.
        let sample_png = {
            let mut buf = Vec::new();
            let mut enc = png::Encoder::new(&mut buf, 4, 4);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[240u8; 4 * 4 * 3]).unwrap();
            drop(w);
            base64_encode(&buf)
        };

        let server = thread::spawn(move || {
            for incoming in listener.incoming().take(4) {
                let Ok(mut stream) = incoming else { break };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                let mut req = Vec::new();
                let mut tmp = [0u8; 4096];
                let end = loop {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break 0;
                    }
                    req.extend_from_slice(&tmp[..n]);
                    if let Some(e) = find_header_end(&req) {
                        break e;
                    }
                };
                if end == 0 {
                    continue;
                }
                let hdr = String::from_utf8_lossy(&req[..end]).into_owned();
                let first_line = hdr.lines().next().unwrap_or("");
                if first_line.starts_with("GET /json/list ") {
                    let body = json!([{
                        "id": "PAGE1",
                        "title": "Mock Page",
                        "url": "https://example.test",
                        "type": "page",
                        "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/page/PAGE1")
                    }])
                    .to_string();
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                    let _ = stream.write_all(resp.as_bytes());
                    continue;
                }
                if first_line.starts_with("GET /devtools/page/PAGE1 ") {
                    let key = hdr
                        .lines()
                        .find_map(|l| l.split_once(':').and_then(|(k, v)| k.eq_ignore_ascii_case("sec-websocket-key").then(|| v.trim())))
                        .unwrap();
                    let accept = ws_accept_key(key);
                    let resp = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n");
                    let _ = stream.write_all(resp.as_bytes());
                    // Send a server ping frame immediately to verify client replies with pong.
                    let _ = stream.write_all(&[0x89, 0x02, b'h', b'i']);

                    let mut read_buf = req[end..].to_vec();
                    let read_exact = |s: &mut TcpStream, buf: &mut Vec<u8>, n: usize| -> Option<Vec<u8>> {
                        while buf.len() < n {
                            let mut t = [0u8; 4096];
                            let got = s.read(&mut t).ok()?;
                            if got == 0 {
                                return None;
                            }
                            buf.extend_from_slice(&t[..got]);
                        }
                        Some(buf.drain(..n).collect())
                    };
                    let send_ws_text = |s: &mut TcpStream, txt: &str, fragment: bool| {
                        let bytes = txt.as_bytes();
                        if fragment && bytes.len() > 10 {
                            let mid = bytes.len() / 2;
                            for (i, part) in [&bytes[..mid], &bytes[mid..]].iter().enumerate() {
                                let b0 = if i == 0 { 0x01 } else { 0x80 };
                                let mut frame = vec![b0];
                                if part.len() < 126 {
                                    frame.push(part.len() as u8);
                                } else {
                                    frame.push(126);
                                    frame.extend_from_slice(&(part.len() as u16).to_be_bytes());
                                }
                                frame.extend_from_slice(part);
                                let _ = s.write_all(&frame);
                            }
                        } else {
                            let mut frame = vec![0x81];
                            if bytes.len() < 126 {
                                frame.push(bytes.len() as u8);
                            } else {
                                frame.push(126);
                                frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                            }
                            frame.extend_from_slice(bytes);
                            let _ = s.write_all(&frame);
                        }
                    };

                    while let Some(h) = read_exact(&mut stream, &mut read_buf, 2) {
                        let opcode = h[0] & 0x0F;
                        let masked = (h[1] & 0x80) != 0;
                        let len = match h[1] & 0x7F {
                            126 => {
                                let ext = read_exact(&mut stream, &mut read_buf, 2).unwrap();
                                u16::from_be_bytes([ext[0], ext[1]]) as usize
                            }
                            127 => {
                                let ext = read_exact(&mut stream, &mut read_buf, 8).unwrap();
                                u64::from_be_bytes(ext.try_into().unwrap()) as usize
                            }
                            n => n as usize,
                        };
                        let mask = if masked { Some(read_exact(&mut stream, &mut read_buf, 4).unwrap()) } else { None };
                        let mut payload = read_exact(&mut stream, &mut read_buf, len).unwrap();
                        if let Some(m) = mask {
                            for (i, b) in payload.iter_mut().enumerate() {
                                *b ^= m[i & 3];
                            }
                        }
                        if opcode == 0xA {
                            continue; // Pong
                        }
                        if opcode == 0x8 {
                            break;
                        }
                        let msg: Value = serde_json::from_slice(&payload).unwrap();
                        let id = msg["id"].as_u64().unwrap();
                        let method = msg["method"].as_str().unwrap().to_string();
                        recorded_srv.lock().unwrap().push(method.clone());

                        // Interleave an async CDP event before replying.
                        send_ws_text(&mut stream, &json!({ "method": "DOM.documentUpdated", "params": {} }).to_string(), false);

                        let result = match method.as_str() {
                            "Runtime.evaluate" => {
                                let expr = msg["params"]["expression"].as_str().unwrap_or("");
                                if expr.contains("MAX_NODES") {
                                    let is_on = clicked_srv.load(Ordering::Relaxed);
                                    json!({ "result": { "value": {
                                        "title": "Mock Page",
                                        "url": "https://example.test",
                                        "viewport": [800, 600],
                                        "hidden": 0,
                                        "nodes": [
                                            { "dom_id": 1, "identity": "button|#go", "role": "button", "name": "Submit", "value": null, "states": if is_on { vec!["selected"] } else { vec![] }, "cells": [], "rect": [20, 30, 100, 40], "tag": "button", "is_password": false, "disabled": false, "readonly": false, "editable": false, "checkable": false, "selectable": false },
                                            { "dom_id": 2, "identity": "edit|#pw", "role": "edit", "name": "Secret", "value": "hidden", "states": [], "cells": [], "rect": [20, 80, 120, 30], "tag": "input", "is_password": true, "disabled": false, "readonly": false, "editable": true, "checkable": false, "selectable": false },
                                            { "dom_id": 3, "identity": "table|#t", "role": "table", "name": "Prices", "value": null, "states": [], "cells": [], "rect": [20, 130, 300, 100], "tag": "table", "is_password": false, "disabled": false, "readonly": false, "editable": false, "checkable": false, "selectable": false }
                                        ]
                                    } } })
                                } else if expr.contains("scrollIntoView") && expr.contains("frameElement") {
                                    json!({ "result": { "value": { "x": 70.0, "y": 50.0 } } })
                                } else if expr.contains("querySelectorAll('tr") {
                                    json!({ "result": { "value": { "total_rows": 2, "total_columns": 2, "headers": ["Item", "Cost"], "rows": [["Tea", "3"], ["Cake", "5"]] } } })
                                } else {
                                    json!({ "result": { "value": { "ok": true, "text": "hello" } } })
                                }
                            }
                            "Input.dispatchMouseEvent" => {
                                if msg["params"]["type"] == "mouseReleased" {
                                    clicked_srv.store(true, Ordering::Relaxed);
                                }
                                json!({})
                            }
                            "Page.captureScreenshot" => json!({ "data": sample_png }),
                            _ => json!({}),
                        };
                        send_ws_text(&mut stream, &json!({ "id": id, "result": result }).to_string(), id == 1);
                    }
                }
            }
        });

        let hub = CdpHub::new_isolated();
        let connected = hub.command("tester", &[port.to_string()]).unwrap();
        assert!(connected.contains("1 pages"), "{connected}");

        // Explicit CDP mode MUST NOT hijack non-browser native windows like Calculator or Notepad!
        assert!(hub.target_for_window("Calculator", "CalculatorApp", false).is_none());
        assert!(hub.target_for_window("Mock Page - Notepad", "notepad", false).is_none());

        let target = hub.target_for_window("Mock Page - Google Chrome", "chrome", false).unwrap();
        assert_eq!(target.id, "PAGE1");

        // Electron apps launched with --cdp auto-attach even if not in the default browser list.
        hub.register_port(port, "code");
        assert!(hub.target_for_window("Workspace - Visual Studio Code", "code", false).is_some());

        let first = hub.see("tester", "w1", &target, &ActionFlags::default()).unwrap();
        assert!(first.contains("[cdp] 3 controls"), "{first}");
        assert!(first.contains("e1 button \"Submit\""), "{first}");
        assert!(!first.contains("hidden"), "password value must not appear in see");

        assert_eq!(hub.see("tester", "w1", &target, &ActionFlags::default()).unwrap(), "no change");
        assert!(hub.get(&target, "e2").unwrap_err().contains("password"));

        let tbl = hub.table(&target, "e3", 0).unwrap();
        assert!(tbl.contains("2 rows, 2 columns") && tbl.contains("Tea\t3"), "{tbl}");

        let stop = AtomicBool::new(false);
        let click_out = hub
            .run_steps(
                "tester",
                &target,
                &ActionFlags::default(),
                &[ActionStep { flags: ActionFlags::default(), words: vec!["click".into(), "Submit".into()] }],
                &stop,
            )
            .unwrap();
        assert!(click_out.contains("~ e1 button \"Submit\" [selected]"), "{click_out}");
        assert_eq!(hub.is_state(&target, "e1", "selected").unwrap(), "yes");
        assert_eq!(hub.is_state(&target, "e1", "disabled").unwrap(), "no");

        // Emergency stop flag aborts run_steps immediately.
        let halted = AtomicBool::new(true);
        let stop_err = hub
            .run_steps(
                "tester",
                &target,
                &ActionFlags::default(),
                &[ActionStep { flags: ActionFlags::default(), words: vec!["wait".into(), "5000".into()] }],
                &halted,
            )
            .unwrap_err();
        assert!(stop_err.contains("stopped"), "{stop_err}");

        let shot_out = hub.shot("tester", &target, None, &ActionFlags::default()).unwrap();
        assert!(shot_out.contains("4x4") && shot_out.contains("[cdp]"), "{shot_out}");
        let shot_path = shot_out.split_whitespace().next().unwrap();
        let _ = fs::remove_file(shot_path);

        let methods = recorded.lock().unwrap().clone();
        assert!(methods.contains(&"Runtime.evaluate".to_string()));
        assert!(methods.contains(&"Input.dispatchMouseEvent".to_string()));
        assert!(methods.contains(&"Page.captureScreenshot".to_string()));
        drop(hub);
        let _ = server.join();
    }

    #[test]
    fn cdp_command_toggles_and_rejects_bad_urls() {
        let hub = CdpHub::new_isolated();
        assert!(hub.is_enabled());
        assert!(hub.command("t", &["off".into()]).unwrap().starts_with("cdp: off"));
        assert!(!hub.is_enabled());
        assert_eq!(hub.command("t", &["on".into()]).unwrap(), "cdp: on");
        assert!(hub.is_enabled());
        let err = hub.command("t", &["goto".into(), "javascript:alert(1)".into()]).unwrap_err();
        assert!(err.contains("http://"), "{err}");
        let err2 = hub.command("t", &["http://example.com:9222".into()]).unwrap_err();
        assert!(err2.contains("loopback"), "{err2}");
    }

    #[test]
    fn ipv6_browser_detection_risky_chords_and_targets_hardening() {
        // IPv6 display keeps brackets; localhost is preserved; whitespace in path is rejected.
        let v6 = parse_endpoint("http://[::1]:9222").unwrap();
        assert_eq!(v6.display(), "http://[::1]:9222");
        let v6_ws = parse_endpoint("ws://[::1]:9222/devtools/page/1").unwrap();
        assert_eq!(v6_ws.display(), "ws://[::1]:9222/devtools/page/1");
        let lh = parse_endpoint("http://localhost:9222").unwrap();
        assert_eq!(lh.display(), "http://localhost:9222");
        assert!(parse_endpoint("ws://127.0.0.1:9222/bad path").is_err());

        // Browser app detection avoids false prefix matches.
        assert!(is_browser_app("chrome.exe"));
        assert!(is_browser_app("Google Chrome.app"));
        assert!(is_browser_app("msedge"));
        assert!(is_browser_app("arc"));
        assert!(!is_browser_app("archive"));
        assert!(!is_browser_app("archiver"));
        assert!(!is_browser_app("operator"));
        assert!(!is_browser_app("notepad.exe"));

        // Direct specs
        assert!(is_direct_spec("cdp"));
        assert!(is_direct_spec("t1"));
        assert!(is_direct_spec("cdp:example"));
        assert!(!is_direct_spec("w1"));
        assert!(!is_direct_spec("t0"));

        // Normalized risky chords (including alt+f04 and tab/browser close shortcuts).
        assert!(is_risky_cdp_chord("alt+f04"));
        assert!(is_risky_cdp_chord("ctrl+w"));
        assert!(is_risky_cdp_chord("cmd+w"));
        assert!(is_risky_cdp_chord("ctrl+q"));
        assert!(is_risky_cdp_chord("cmd+option+esc"));
        assert!(!is_risky_cdp_chord("ctrl+c"));

        // parse_targets_json rejects non-ws:// URLs even on loopback.
        let raw = json!([
            { "id": "1", "type": "page", "title": "Bad", "url": "https://a", "webSocketDebuggerUrl": "http://127.0.0.1:9222/foo" },
            { "id": "2", "type": "page", "title": "Good", "url": "https://b", "webSocketDebuggerUrl": "ws://127.0.0.1:9222/devtools/page/2" }
        ])
        .to_string();
        let parsed = parse_targets_json(&raw).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "2");
    }

    #[test]
    #[ignore = "launches a local headless Chrome/Edge process for live CDP DOM verification"]
    fn headless_chromium_live_smoke() {
        let candidates: &[&str] = if cfg!(windows) {
            &[
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
                r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            ]
        } else if cfg!(target_os = "macos") {
            &[
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            ]
        } else {
            &["/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser"]
        };
        let browser = candidates
            .iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("no Chromium/Edge binary found for headless smoke test");

        let port = allocate_port().unwrap();
        let profile = std::env::temp_dir().join(format!("deskcrew-cdp-smoke-{}-{}", std::process::id(), port));
        let _ = fs::create_dir_all(&profile);

        // Serve a local HTML page over loopback so iframe/origin/DOM behavior is realistic.
        let html = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>CDP Smoke Test</title></head>
<body>
  <h1>CDP Playground</h1>
  <label for="name">Full Name</label>
  <input id="name" type="text" value="initial">
  <label for="secret">Password</label>
  <input id="secret" type="password" value="s3cr3t">
  <label><input id="agree" type="checkbox"> Accept terms</label>
  <label for="country">Country</label>
  <select id="country"><option value="jp">Japan</option><option value="br">Brazil</option></select>
  <details id="det"><summary>More info</summary><p>Hidden details</p></details>
  <button id="go">Submit</button>
  <div id="status" role="status">ready</div>
  <table aria-label="Inventory">
    <thead><tr><th>Item</th><th>Qty</th></tr></thead>
    <tbody><tr><td>Apples</td><td>4</td></tr><tr><td>Pears</td><td>9</td></tr></tbody>
  </table>
  <div id="shadow-host"></div>
  <iframe id="fr" srcdoc="<!doctype html><html><body><button id='ib' onclick='this.textContent=&quot;Framed OK&quot;'>Frame Click</button></body></html>" width="180" height="50"></iframe>
  <canvas id="pad" aria-label="Sketchpad" width="160" height="80" style="border:1px solid #000"></canvas>
  <script>
    document.getElementById('go').addEventListener('click', () => {
      const n = document.getElementById('name').value;
      const c = document.getElementById('country').value;
      const a = document.getElementById('agree').checked;
      document.getElementById('status').textContent = `submitted:${n}:${c}:${a}`;
    });
    const host = document.getElementById('shadow-host');
    const sr = host.attachShadow({ mode: 'open' });
    const sb = document.createElement('button');
    sb.textContent = 'Shadow Ping';
    sb.addEventListener('click', () => { sb.textContent = 'Shadow Pong'; });
    sr.appendChild(sb);
    const pad = document.getElementById('pad');
    let strokes = 0;
    pad.addEventListener('mousedown', () => { strokes++; pad.setAttribute('aria-label', 'Sketchpad ' + strokes); });
  </script>
</body></html>"#;
        let html_file = profile.join("index.html");
        fs::write(&html_file, html).unwrap();
        let file_url = format!("file:///{}", html_file.to_string_lossy().replace('\\', "/").trim_start_matches('/'));

        struct Cleanup {
            child: std::process::Child,
            dir: PathBuf,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let _ = fs::remove_dir_all(&self.dir);
            }
        }

        let child = std::process::Command::new(browser)
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--window-size=1024,768")
            .arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(&file_url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn headless browser");
        let _cleanup = Cleanup { child, dir: profile.clone() };

        let hub = CdpHub::new_isolated();
        let mut connected = Err("not yet".to_string());
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(100));
            let res = hub.command("smoke", &[port.to_string()]);
            if let Ok(ref s) = res
                && !s.contains("0 pages")
            {
                connected = res;
                break;
            }
        }
        let connected = connected.expect("headless Chrome did not expose CDP page");
        assert!(connected.contains("pages"), "{connected}");

        let target = hub.pick_direct_spec("smoke", "cdp").unwrap().unwrap();
        let seen = hub.see("smoke", "cdp", &target, &ActionFlags::default()).unwrap();
        println!("--- INITIAL SEE ---\n{seen}");
        assert!(seen.contains("[cdp]"), "{seen}");
        assert!(seen.contains("Full Name"), "{seen}");
        assert!(seen.contains("Password"), "{seen}");
        assert!(!seen.contains("s3cr3t"), "password value leaked in see:\n{seen}");
        assert!(seen.contains("Accept terms"), "{seen}");
        assert!(seen.contains("Country"), "{seen}");
        assert!(seen.contains("Submit"), "{seen}");
        assert!(seen.contains("Shadow Ping"), "shadow DOM button missing:\n{seen}");
        assert!(seen.contains("Frame Click"), "same-origin iframe button missing:\n{seen}");
        assert!(seen.contains("Sketchpad"), "canvas missing:\n{seen}");
        assert!(!seen.contains("Hidden details"), "closed <details> body should stay hidden:\n{seen}");

        // get refuses password, returns normal input value
        assert!(hub.get(&target, "Password").unwrap_err().contains("password"));
        assert_eq!(hub.get(&target, "Full Name").unwrap(), "initial");

        // table reads rows & headers
        let tbl = hub.table(&target, "Inventory", 0).unwrap();
        println!("--- TABLE ---\n{tbl}");
        assert!(tbl.contains("Apples\t4") && tbl.contains("Pears\t9"), "{tbl}");

        // Multi-step do: set name, check checkbox, select Brazil, expand details, click Submit, click Shadow Ping, click Frame Click, draw on canvas
        let steps = vec![
            ActionStep { flags: ActionFlags::default(), words: vec!["set".into(), "Full Name".into(), "Ada Lovelace".into()] },
            ActionStep { flags: ActionFlags::default(), words: vec!["check".into(), "Accept terms".into()] },
            ActionStep { flags: ActionFlags::default(), words: vec!["select".into(), "Country".into(), "Brazil".into()] },
            ActionStep { flags: ActionFlags::default(), words: vec!["expand".into(), "More info".into()] },
            ActionStep { flags: ActionFlags::default(), words: vec!["click".into(), "Submit".into()] },
            ActionStep { flags: ActionFlags::default(), words: vec!["click".into(), "Shadow Ping".into()] },
            ActionStep { flags: ActionFlags::default(), words: vec!["click".into(), "Frame Click".into()] },
            ActionStep { flags: ActionFlags::default(), words: vec!["draw".into(), "Sketchpad".into(), "10,10 60,40".into()] },
        ];
        let stop = AtomicBool::new(false);
        let do_out = hub.run_steps("smoke", &target, &ActionFlags::default(), &steps, &stop).unwrap();
        println!("--- DO OUTPUT ---\n{do_out}");
        assert!(do_out.contains("submitted:Ada Lovelace:br:true"), "{do_out}");
        assert!(do_out.contains("Hidden details"), "expanded <details> body should appear:\n{do_out}");
        assert!(do_out.contains("Shadow Pong"), "{do_out}");
        assert!(do_out.contains("Framed OK"), "{do_out}");
        assert!(do_out.contains("Sketchpad 1"), "{do_out}");

        // is state checks
        assert_eq!(hub.is_state(&target, "Accept terms", "on").unwrap(), "yes");
        assert_eq!(hub.is_state(&target, "More info", "open").unwrap(), "yes");
        assert_eq!(hub.is_state(&target, "Submit", "enabled").unwrap(), "yes");

        // shot produces a real PNG
        let shot_out = hub.shot("smoke", &target, None, &ActionFlags::default()).unwrap();
        println!("--- SHOT ---\n{shot_out}");
        assert!(shot_out.contains("[cdp]"), "{shot_out}");
        let shot_path = shot_out.split_whitespace().next().unwrap();
        assert!(fs::metadata(shot_path).unwrap().len() > 500);
        let _ = fs::remove_file(shot_path);
    }
}
