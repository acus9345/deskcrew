// The local socket between the short-lived command and the service that stays up. It lives in the
// person's own temporary folder, which no other user can open.
use std::{
    io::{self, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::Duration,
};
#[cfg(target_os = "linux")]
use std::os::unix::fs::{DirBuilderExt, MetadataExt};

const MAX_MESSAGE: usize = 1 << 24;
const CONNECT_WAIT: Duration = Duration::from_secs(120);
#[cfg(target_os = "linux")]
const MAX_SUN_PATH: usize = 107;
#[cfg(not(target_os = "linux"))]
const MAX_SUN_PATH: usize = 103;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

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
fn is_private_dir(dir: &Path, uid: u32) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else { return false };
    meta.is_dir() && !meta.file_type().is_symlink() && meta.uid() == uid && (meta.mode() & 0o077) == 0
}

#[cfg(target_os = "linux")]
fn ensure_private_dir(dir: &Path, uid: u32, create: bool) -> io::Result<()> {
    if std::fs::symlink_metadata(dir).is_err() {
        if !create {
            return Err(io::Error::new(io::ErrorKind::NotFound, "socket folder missing"));
        }
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    if !is_private_dir(dir, uid) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsafe socket folder permissions"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn socket_name(short: bool) -> String {
    let raw = std::env::var("DISPLAY")
        .ok()
        .filter(|d| !d.is_empty())
        .or_else(|| std::env::var("WAYLAND_DISPLAY").ok().filter(|d| !d.is_empty()))
        .unwrap_or_default();
    let display = if raw.is_empty() {
        String::new()
    } else if short {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        raw.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    } else {
        let sanitized: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        if sanitized.len() > 24 {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            raw.hash(&mut hasher);
            format!("{}-{:016x}", &sanitized[..12], hasher.finish())
        } else {
            sanitized
        }
    };
    format!("deskcrew-{}{}{display}.sock", env!("CARGO_PKG_VERSION"), if display.is_empty() { "" } else { "-" })
}

fn path_for(create: bool) -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let uid = current_uid().ok_or_else(|| io::Error::other("could not determine user id"))?;
        let (primary_dir, needs_create) = if let Some(xdg) = env_path("XDG_RUNTIME_DIR")
            && is_private_dir(&xdg, uid)
        {
            (xdg, false)
        } else {
            let base = env_path("TMPDIR").unwrap_or_else(std::env::temp_dir);
            (base.join(format!("deskcrew-{uid}")), true)
        };
        for short in [false, true] {
            let candidate = primary_dir.join(socket_name(short));
            if candidate.as_os_str().as_encoded_bytes().len() <= MAX_SUN_PATH {
                if needs_create {
                    ensure_private_dir(&primary_dir, uid, create)?;
                }
                return Ok(candidate);
            }
        }
        let fallback_dir = PathBuf::from("/tmp").join(format!("deskcrew-{uid}"));
        let fallback = fallback_dir.join(socket_name(true));
        if fallback.as_os_str().as_encoded_bytes().len() <= MAX_SUN_PATH {
            ensure_private_dir(&fallback_dir, uid, create)?;
            return Ok(fallback);
        }
        Err(io::Error::new(io::ErrorKind::InvalidInput, "socket path exceeds Unix socket length limit"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = create;
        let folder = env_path("TMPDIR").unwrap_or_else(std::env::temp_dir);
        let name = format!("deskcrew-{}.sock", env!("CARGO_PKG_VERSION"));
        let candidate = folder.join(&name);
        if candidate.as_os_str().as_encoded_bytes().len() <= MAX_SUN_PATH {
            return Ok(candidate);
        }
        Ok(PathBuf::from("/tmp").join(name))
    }
}

fn lock_for(path: &Path) -> io::Result<std::fs::File> {
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    if let Ok(meta) = std::fs::symlink_metadata(&lock_path)
        && meta.file_type().is_symlink()
    {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsafe socket lock symlink"));
    }
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).open(&lock_path)?;
    file.lock()?;
    Ok(file)
}

pub struct Pipe(UnixStream);

impl Pipe {
    /// Own the socket's name. A name left by a service that died is taken over.
    pub fn bind() -> io::Result<UnixListener> {
        let path = path_for(true)?;
        let _lock = lock_for(&path)?;
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
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsafe socket symlink"));
        }
        #[cfg(target_os = "linux")]
        if let Some(uid) = current_uid()
            && meta.uid() != uid
        {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsafe socket owner"));
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
        let _lock = lock_for(&path).ok();
        let _ = std::fs::remove_file(path);
    }
}
