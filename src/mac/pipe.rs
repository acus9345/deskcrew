// The local socket between the short-lived command and the service that stays up. It lives in the
// person's own temporary folder, which no other user can open.
use std::{
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    time::Duration,
};
#[cfg(target_os = "linux")]
use std::os::unix::fs::{DirBuilderExt, MetadataExt};

const MAX_MESSAGE: usize = 1 << 24;
const CONNECT_WAIT: Duration = Duration::from_secs(120);

#[cfg(target_os = "linux")]
fn current_uid() -> Option<u32> {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                let mut parts = rest.split_whitespace();
                let _real = parts.next();
                if let Some(euid) = parts.next().and_then(|s| s.parse::<u32>().ok()) {
                    return Some(euid);
                }
            }
        }
    }
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(target_os = "linux")]
fn is_private_dir(dir: &std::path::Path, uid: u32) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else { return false };
    meta.is_dir() && !meta.file_type().is_symlink() && meta.uid() == uid && (meta.mode() & 0o077) == 0
}

#[cfg(target_os = "linux")]
fn socket_dir(create: bool) -> io::Result<PathBuf> {
    let uid = current_uid().ok_or_else(|| io::Error::other("could not determine user id"))?;
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
        && is_private_dir(&xdg, uid)
    {
        return Ok(xdg);
    }
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("deskcrew-{uid}"));
    if std::fs::symlink_metadata(&dir).is_err() {
        if !create {
            return Err(io::Error::new(io::ErrorKind::NotFound, "socket folder missing"));
        }
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    if !is_private_dir(&dir, uid) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsafe socket folder permissions"));
    }
    Ok(dir)
}

#[cfg(target_os = "linux")]
fn display_tag() -> String {
    let raw = std::env::var("DISPLAY")
        .ok()
        .filter(|d| !d.is_empty())
        .or_else(|| std::env::var("WAYLAND_DISPLAY").ok().filter(|d| !d.is_empty()))
        .unwrap_or_default();
    let sanitized: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    if sanitized.len() > 24 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        raw.hash(&mut hasher);
        format!("{}-{:016x}", &sanitized[..12], hasher.finish())
    } else {
        sanitized
    }
}

fn path_for(create: bool) -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let folder = socket_dir(create)?;
        let display = display_tag();
        Ok(folder.join(format!("deskcrew-{}{}{display}.sock", env!("CARGO_PKG_VERSION"), if display.is_empty() { "" } else { "-" })))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = create;
        let folder = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        Ok(folder.join(format!("deskcrew-{}.sock", env!("CARGO_PKG_VERSION"))))
    }
}

pub struct Pipe(UnixStream);

impl Pipe {
    /// Own the socket's name. A name left by a service that died is taken over.
    pub fn bind() -> io::Result<UnixListener> {
        let path = path_for(true)?;
        if UnixStream::connect(&path).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, "another deskcrew service is running"));
        }
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }

    pub fn from_stream(stream: UnixStream) -> Pipe {
        Pipe(stream)
    }

    pub fn connect() -> io::Result<Pipe> {
        let path = path_for(false)?;
        #[cfg(target_os = "linux")]
        if let Some(uid) = current_uid() {
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() || meta.uid() != uid {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsafe socket owner"));
            }
        }
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(CONNECT_WAIT))?;
        Ok(Pipe(stream))
    }

    pub fn send(&self, message: &[u8]) -> io::Result<()> {
        let mut stream = &self.0;
        stream.write_all(&(message.len() as u32).to_le_bytes())?;
        stream.write_all(message)
    }

    pub fn receive(&self) -> io::Result<Vec<u8>> {
        let mut stream = &self.0;
        let mut size = [0; 4];
        stream.read_exact(&mut size)?;
        let size = u32::from_le_bytes(size) as usize;
        if size > MAX_MESSAGE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
        }
        let mut message = vec![0; size];
        stream.read_exact(&mut message)?;
        Ok(message)
    }

    /// Let the reply drain before the socket closes.
    pub fn finish(&self) {
        let _ = (&self.0).flush();
        let _ = self.0.shutdown(std::net::Shutdown::Write);
    }
}

pub fn remove() {
    if let Ok(path) = path_for(false) {
        let _ = std::fs::remove_file(path);
    }
}
