// The local pipe between the short-lived command and the service that stays up.
use std::{io, ptr::null};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
        WriteFile,
    },
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
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

fn desktop() -> String {
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

pub struct Pipe(HANDLE);

// One thread uses a pipe at a time; it only crosses to the thread that serves it.
unsafe impl Send for Pipe {}

impl Pipe {
    /// Listen for the next command. The first caller owns the name, so a second service cannot take it.
    pub fn listen(first: bool) -> io::Result<Pipe> {
        let open = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        let handle = unsafe { CreateNamedPipeW(name().as_ptr(), open, mode, PIPE_UNLIMITED_INSTANCES, BUFFER, BUFFER, 0, null()) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Pipe(handle))
    }

    /// Wait for a command to connect. A client already connected counts.
    pub fn accept(&self) -> bool {
        let ok = unsafe { ConnectNamedPipe(self.0, std::ptr::null_mut()) };
        ok != 0 || unsafe { GetLastError() } == 535
    }

    pub fn connect() -> io::Result<Pipe> {
        let name = name();
        for _ in 0..50 {
            let handle = unsafe {
                CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, 0, std::ptr::null_mut())
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(Pipe(handle));
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
            let ok = unsafe { ReadFile(self.0, buffer.as_mut_ptr(), buffer.len() as u32, &mut got, std::ptr::null_mut()) };
            if ok == 0 || got == 0 {
                return Err(io::Error::last_os_error());
            }
            buffer = &mut buffer[got as usize..];
        }
        Ok(())
    }

    fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let mut put = 0;
            let ok = unsafe { WriteFile(self.0, bytes.as_ptr(), bytes.len() as u32, &mut put, std::ptr::null_mut()) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            bytes = &bytes[put as usize..];
        }
        Ok(())
    }

    pub fn send(&self, message: &[u8]) -> io::Result<()> {
        self.write_all(&(message.len() as u32).to_le_bytes())?;
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
