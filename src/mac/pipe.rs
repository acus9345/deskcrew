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

const MAX_MESSAGE: usize = 1 << 24;
const CONNECT_WAIT: Duration = Duration::from_secs(120);

fn path() -> PathBuf {
    let folder = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    // On Linux each X display (a desktop, or an Xvfb) gets its own service.
    #[cfg(target_os = "linux")]
    let display: String = std::env::var("DISPLAY").unwrap_or_default().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    #[cfg(not(target_os = "linux"))]
    let display = String::new();
    folder.join(format!("deskcrew-{}{}{display}.sock", env!("CARGO_PKG_VERSION"), if display.is_empty() { "" } else { "-" }))
}

pub struct Pipe(UnixStream);

impl Pipe {
    /// Own the socket's name. A name left by a service that died is taken over.
    pub fn bind() -> io::Result<UnixListener> {
        let path = path();
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
        let stream = UnixStream::connect(path())?;
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
    let _ = std::fs::remove_file(path());
}
