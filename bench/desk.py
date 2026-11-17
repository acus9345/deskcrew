"""Run programs on a hidden Windows desktop, so nothing they do can reach the person's screen,
focus, mouse or keyboard."""
import ctypes, ctypes.wintypes as wt, os, subprocess, time

k32 = ctypes.WinDLL('kernel32', use_last_error=True)
u32 = ctypes.WinDLL('user32', use_last_error=True)

GENERIC_ALL = 0x10000000
u32.CreateDesktopW.restype = wt.HANDLE
u32.CreateDesktopW.argtypes = [wt.LPCWSTR, wt.LPCWSTR, ctypes.c_void_p, wt.DWORD, wt.DWORD, ctypes.c_void_p]


class SECURITY_ATTRIBUTES(ctypes.Structure):
    _fields_ = [('nLength', wt.DWORD), ('lpSecurityDescriptor', ctypes.c_void_p), ('bInheritHandle', wt.BOOL)]


k32.CreateFileW.restype = wt.HANDLE
k32.CreateFileW.argtypes = [wt.LPCWSTR, wt.DWORD, wt.DWORD, ctypes.c_void_p, wt.DWORD, wt.DWORD, wt.HANDLE]
k32.CloseHandle.argtypes = [wt.HANDLE]
k32.WaitForSingleObject.argtypes = [wt.HANDLE, wt.DWORD]
k32.WaitForSingleObject.restype = wt.DWORD
k32.GetExitCodeProcess.argtypes = [wt.HANDLE, ctypes.POINTER(wt.DWORD)]
k32.CreateProcessW.argtypes = [wt.LPCWSTR, wt.LPWSTR, ctypes.c_void_p, ctypes.c_void_p, wt.BOOL, wt.DWORD,
                               ctypes.c_void_p, wt.LPCWSTR, ctypes.c_void_p, ctypes.c_void_p]
u32.CloseDesktop.argtypes = [wt.HANDLE]


class STARTUPINFO(ctypes.Structure):
    _fields_ = [('cb', wt.DWORD), ('lpReserved', wt.LPWSTR), ('lpDesktop', wt.LPWSTR), ('lpTitle', wt.LPWSTR),
                ('dwX', wt.DWORD), ('dwY', wt.DWORD), ('dwXSize', wt.DWORD), ('dwYSize', wt.DWORD),
                ('dwXCountChars', wt.DWORD), ('dwYCountChars', wt.DWORD), ('dwFillAttribute', wt.DWORD),
                ('dwFlags', wt.DWORD), ('wShowWindow', wt.WORD), ('cbReserved2', wt.WORD),
                ('lpReserved2', ctypes.c_void_p), ('hStdInput', wt.HANDLE), ('hStdOutput', wt.HANDLE), ('hStdError', wt.HANDLE)]


class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [('hProcess', wt.HANDLE), ('hThread', wt.HANDLE), ('dwProcessId', wt.DWORD), ('dwThreadId', wt.DWORD)]


class Desktop:
    def __init__(self, name='deskcrewbench'):
        self.name = name
        if name == 'Default':  # the person's own desktop, for tools that need real input
            self.handle = None
            return
        self.handle = u32.CreateDesktopW(name, None, None, 0, GENERIC_ALL, None)
        if not self.handle:
            raise OSError(ctypes.get_last_error(), 'CreateDesktop failed')

    def start(self, cmdline, cwd=None, env=None, out=None):
        """Start a process on this desktop. Output goes to the file `out`, if given."""
        si = STARTUPINFO(); si.cb = ctypes.sizeof(si); si.lpDesktop = 'WinSta0\\' + self.name
        handle = None
        if out:
            sa = SECURITY_ATTRIBUTES(ctypes.sizeof(SECURITY_ATTRIBUTES), None, True)
            handle = k32.CreateFileW(out, 0x40000000, 3, ctypes.byref(sa), 2, 0x80, None)
            nul = k32.CreateFileW('NUL', 0x80000000, 3, ctypes.byref(sa), 3, 0x80, None)
            si.dwFlags |= 0x100  # STARTF_USESTDHANDLES
            si.hStdInput, si.hStdOutput, si.hStdError = nul, handle, handle
        pi = PROCESS_INFORMATION()
        block = None
        if env is not None:
            block = ctypes.create_unicode_buffer(''.join(f'{k}={v}\0' for k, v in env.items()) + '\0')
        flags = 0x08000000 | 0x400  # CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT
        ok = k32.CreateProcessW(None, ctypes.create_unicode_buffer(cmdline), None, None, bool(out), flags,
                                block, cwd, ctypes.byref(si), ctypes.byref(pi))
        if out:
            k32.CloseHandle(handle)
        if not ok:
            raise OSError(ctypes.get_last_error(), 'CreateProcess failed: ' + cmdline)
        k32.CloseHandle(pi.hThread)
        return Proc(pi.hProcess, pi.dwProcessId)

    def run(self, cmdline, cwd=None, env=None, out=None, timeout=600):
        p = self.start(cmdline, cwd, env, out)
        return p.wait(timeout)

    def close(self):
        if self.handle:
            u32.CloseDesktop(self.handle)


class Proc:
    def __init__(self, handle, pid):
        self.handle, self.pid = handle, pid

    def wait(self, timeout):
        r = k32.WaitForSingleObject(self.handle, int(timeout * 1000))
        if r != 0:
            self.kill()
            return None
        code = wt.DWORD(); k32.GetExitCodeProcess(self.handle, ctypes.byref(code))
        return code.value

    def alive(self):
        return k32.WaitForSingleObject(self.handle, 0) == 0x102

    def kill(self):
        subprocess.run(['taskkill', '/T', '/F', '/PID', str(self.pid)], capture_output=True)


if __name__ == '__main__':
    import sys, tempfile
    d = Desktop()
    out = os.path.join(tempfile.gettempdir(), 'desk_test.txt')
    print('exit', d.run(' '.join(sys.argv[1:]), out=out, timeout=60))
    print(open(out, encoding='utf-8', errors='replace').read())
