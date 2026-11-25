// The local pipe between the short-lived command and the service that stays up.
use std::{io, ptr::null};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
        WriteFile,
    },
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE,
            PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
        },
        RemoteDesktop::ProcessIdToSessionId,
        Threading::GetCurrentProcessId,
    },
};

const BUFFER: u32 = 1 << 16;
const MAX_MESSAGE: usize = 1 << 24;

fn name() -> Vec<u16> {
    let mut session = 0;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    // A service only sees the windows of its own desktop, so each desktop gets its own.
    let desktop = match desktop() {
        name if name.is_empty() || name.eq_ignore_ascii_case("Default") => String::new(),
        name => format!("-{name}"),
    };
    format!(r"\\.\pipe\deskcrew-{}-{session}{desktop}", env!("CARGO_PKG_VERSION")).encode_utf16().chain(Some(0)).collect()
}

/// The name of the Windows desktop this process runs on: "Default" for the one on screen. Read
/// once, from the first thread to ask: a thread that has made no window calls has no desktop yet.
pub fn desktop() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(read_desktop).clone()
}

fn read_desktop() -> String {
    use windows_sys::Win32::System::{
        StationsAndDesktops::{GetThreadDesktop, GetUserObjectInformationW, UOI_NAME},
        Threading::GetCurrentThreadId,
    };
    let mut name = [0u16; 256];
    let mut needed = 0;
    let read = unsafe { GetUserObjectInformationW(GetThreadDesktop(GetCurrentThreadId()), UOI_NAME, name.as_mut_ptr().cast(), (name.len() * 2) as u32, &mut needed) };
    if read == 0 {
        return String::new();
    }
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end]).chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

/// The account a process runs as, as the bytes of its security id.
fn account_of(pid: u32) -> Option<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::ERROR_INSUFFICIENT_BUFFER,
        Security::{GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
        System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut token = std::ptr::null_mut();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        CloseHandle(process);
        if opened == 0 || token.is_null() {
            return None;
        }
        // A TOKEN_USER and the SID it points into, aligned for the struct.
        let mut buffer = vec![0u64; 64];
        let mut needed = 0;
        let mut read = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), (buffer.len() * 8) as u32, &mut needed);
        if read == 0 && GetLastError() == ERROR_INSUFFICIENT_BUFFER && needed > 0 && (needed as usize) <= MAX_MESSAGE {
            buffer.resize((needed as usize).div_ceil(8), 0);
            read = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), (buffer.len() * 8) as u32, &mut needed);
        }
        CloseHandle(token);
        if read == 0 {
            return None;
        }
        let sid = (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid;
        if sid.is_null() {
            return None;
        }
        let sid_len = GetLengthSid(sid) as usize;
        if sid_len == 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(sid as *const u8, sid_len).to_vec())
    }
}

fn frame_len(len: usize) -> io::Result<u32> {
    if len > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    u32::try_from(len).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "message too large"))
}

pub struct Pipe(HANDLE);

// One thread uses a pipe at a time; it only crosses to the thread that serves it.
unsafe impl Send for Pipe {}

impl Pipe {
    fn listen_at(pipe_name: &[u16], first: bool) -> io::Result<Pipe> {
        let open = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        let handle = unsafe { CreateNamedPipeW(pipe_name.as_ptr(), open, mode, PIPE_UNLIMITED_INSTANCES, BUFFER, BUFFER, 0, null()) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Pipe(handle))
    }

    /// Listen for the next command. The first caller owns the name, so a second service cannot take it.
    pub fn listen(first: bool) -> io::Result<Pipe> {
        Self::listen_at(&name(), first)
    }

    /// Wait for a command to connect. A client already connected counts; one that disconnected
    /// before `ConnectNamedPipe` is cleared with `DisconnectNamedPipe` so the instance can be reused.
    pub fn accept(&self) -> io::Result<()> {
        loop {
            if unsafe { ConnectNamedPipe(self.0, std::ptr::null_mut()) } != 0 {
                return Ok(());
            }
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => return Ok(()),
                ERROR_NO_DATA => {
                    if unsafe { DisconnectNamedPipe(self.0) } == 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                code => return Err(io::Error::from_raw_os_error(code as i32)),
            }
        }
    }

    pub fn connect() -> io::Result<Pipe> {
        let name = name();
        for _ in 0..50 {
            let handle = unsafe {
                CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, 0, std::ptr::null_mut())
            };
            if handle != INVALID_HANDLE_VALUE {
                let pipe = Pipe(handle);
                // Pipe names are shared by every account on the computer: commands, typed text
                // among them, only go to a service running as this same account.
                let mut server = 0;
                let same = unsafe { GetNamedPipeServerProcessId(handle, &mut server) } != 0 && account_of(server).is_some_and(|theirs| Some(theirs) == account_of(unsafe { GetCurrentProcessId() }));
                if !same {
                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the deskcrew pipe belongs to another account's program: not sending it anything"));
                }
                return Ok(pipe);
            }
            if unsafe { GetLastError() } != ERROR_PIPE_BUSY {
                return Err(io::Error::last_os_error());
            }
            unsafe { WaitNamedPipeW(name.as_ptr(), 100) };
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "the deskcrew service is busy"))
    }

    fn read_exact(&self, mut buffer: &mut [u8]) -> io::Result<()> {
        while !buffer.is_empty() {
            let mut got = 0;
            let chunk = buffer.len().min(u32::MAX as usize) as u32;
            let ok = unsafe { ReadFile(self.0, buffer.as_mut_ptr(), chunk, &mut got, std::ptr::null_mut()) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if got == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "pipe closed"));
            }
            buffer = &mut buffer[got as usize..];
        }
        Ok(())
    }

    fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let mut put = 0;
            let chunk = bytes.len().min(u32::MAX as usize) as u32;
            let ok = unsafe { WriteFile(self.0, bytes.as_ptr(), chunk, &mut put, std::ptr::null_mut()) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if put == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "pipe wrote zero bytes"));
            }
            bytes = &bytes[put as usize..];
        }
        Ok(())
    }

    pub fn send(&self, message: &[u8]) -> io::Result<()> {
        let len = frame_len(message.len())?;
        self.write_all(&len.to_le_bytes())?;
        self.write_all(message)
    }

    pub fn receive(&self) -> io::Result<Vec<u8>> {
        let mut size = [0; 4];
        self.read_exact(&mut size)?;
        let size = u32::from_le_bytes(size) as usize;
        if size > MAX_MESSAGE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
        }
        let mut message = vec![0; size];
        self.read_exact(&mut message)?;
        Ok(message)
    }

    /// The process id of the connected client, learned from Windows over the pipe handle.
    pub fn client_pid(&self) -> Option<u32> {
        let mut pid = 0;
        (unsafe { GetNamedPipeClientProcessId(self.0, &mut pid) } != 0 && pid != 0).then_some(pid)
    }

    /// Let the reply drain before the pipe closes.
    pub fn finish(&self) {
        unsafe {
            FlushFileBuffers(self.0);
            DisconnectNamedPipe(self.0);
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn test_pipe_name(label: &str) -> Vec<u16> {
        format!(r"\\.\pipe\deskcrew-test-{label}-{}", unsafe { GetCurrentProcessId() })
            .encode_utf16()
            .chain(Some(0))
            .collect()
    }

    #[test]
    fn early_client_disconnect_recovers_for_next_client() {
        let pipe_name = test_pipe_name("no-data");
        let server = Pipe::listen_at(&pipe_name, true).unwrap();
        // A client opens and immediately closes before ConnectNamedPipe runs (ERROR_NO_DATA).
        let early = unsafe { CreateFileW(pipe_name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
        assert_ne!(early, INVALID_HANDLE_VALUE);
        drop(Pipe(early));

        let client_name = pipe_name.clone();
        let client = thread::spawn(move || {
            for _ in 0..50 {
                let handle = unsafe { CreateFileW(client_name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
                if handle != INVALID_HANDLE_VALUE {
                    let pipe = Pipe(handle);
                    pipe.send(b"hello").unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("second client could not connect");
        });

        server.accept().unwrap();
        assert_eq!(server.receive().unwrap(), b"hello");
        server.finish();
        client.join().unwrap();
    }

    #[test]
    fn invalid_pipe_returns_error_instead_of_spinning() {
        let bad = Pipe(std::ptr::null_mut());
        assert!(bad.accept().is_err());
        assert_eq!(bad.client_pid(), None);
    }

    #[test]
    fn client_pid_identifies_the_connected_process() {
        let pipe_name = test_pipe_name("client-pid");
        let server = Pipe::listen_at(&pipe_name, true).unwrap();
        let client_name = pipe_name.clone();
        let client = thread::spawn(move || {
            let handle = unsafe { CreateFileW(client_name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
            assert_ne!(handle, INVALID_HANDLE_VALUE);
            let pipe = Pipe(handle);
            pipe.send(b"ping").unwrap();
            let _ = pipe.receive();
        });
        server.accept().unwrap();
        assert_eq!(server.receive().unwrap(), b"ping");
        assert_eq!(server.client_pid(), Some(unsafe { GetCurrentProcessId() }));
        server.send(b"pong").unwrap();
        server.finish();
        client.join().unwrap();
    }

    #[test]
    fn frame_len_checks_max_message_bounds() {
        assert_eq!(frame_len(0).unwrap(), 0);
        assert_eq!(frame_len(MAX_MESSAGE).unwrap(), MAX_MESSAGE as u32);
        let err = frame_len(MAX_MESSAGE + 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
