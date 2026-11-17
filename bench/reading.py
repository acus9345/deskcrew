"""Measure what each tool makes an agent read to see a window, and how long a read takes.

    python bench/reading.py            # every tool, the four windows, three reads each

Opens a throwaway test form, Character Map, Calculator and a File Explorer window on C:\\Windows,
reads each with every tool, prints a table and writes bench/reading.json. It only reads, but
Windows-MCP reads the whole desktop, so run it while you are away from the computer. Windows it
opened are closed at the end; one that was already open is read but left alone.
"""
import ctypes, ctypes.wintypes as wt, json, os, shutil, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, '..'))
sys.path.insert(0, HERE)
import mcpclient, tools  # noqa: E402

READS = 3
user32 = ctypes.windll.user32


def windows():
    found = []
    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def each(hwnd, _):
        if user32.IsWindowVisible(hwnd):
            n = user32.GetWindowTextLengthW(hwnd)
            title = ctypes.create_unicode_buffer(n + 1); user32.GetWindowTextW(hwnd, title, n + 1)
            cls = ctypes.create_unicode_buffer(256); user32.GetClassNameW(hwnd, cls, 256)
            found.append((hwnd, title.value, cls.value))
        return True
    user32.EnumWindows(each, 0)
    return found


def find(title, cls=None):
    for hwnd, t, c in windows():
        if t == title and (cls is None or c == cls):
            return hwnd
    return None


def wait_for(title, cls=None, seconds=15):
    end = time.time() + seconds
    while time.time() < end:
        hwnd = find(title, cls)
        if hwnd:
            time.sleep(1.0)
            return hwnd
        time.sleep(0.2)
    raise SystemExit(f'{title} did not open')


# name, title, window class, how to open it, process name for tools that pick by app
TARGETS = [
    ('Test form', 'bench-read-form', None,
     ['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-STA', '-File', os.path.join(REPO, 'tests', 'form.ps1'),
      '-Title', 'bench-read-form', '-Log', os.path.join(os.environ['TEMP'], 'bench-read-form.log')], 'powershell'),
    ('Character Map', 'Character Map', None, ['charmap.exe'], 'charmap'),
    ('Calculator', 'Calculator', 'ApplicationFrameWindow', ['cmd', '/c', 'start', 'calculator:'], 'Calculator'),
    ('File Explorer', 'Windows - File Explorer', 'CabinetWClass', ['explorer.exe', r'C:\Windows'], 'explorer'),
]


def timed(run):
    start = time.perf_counter()
    text = run()
    return text, (time.perf_counter() - start) * 1000


def cli(exe, args):
    def run():
        out = subprocess.run([exe, *args], capture_output=True, text=True, encoding='utf-8', errors='replace')
        return out.stdout + out.stderr
    return run


def agent_desktop_id(exe, hwnd, title):
    listing = json.loads(subprocess.run([exe, 'list-windows'], capture_output=True, text=True, encoding='utf-8').stdout)
    rows = listing.get('data', listing)
    rows = rows.get('windows', rows) if isinstance(rows, dict) else rows
    for row in rows:
        if row.get('title') == title or str(row.get('id', '')).endswith(str(hwnd)):
            return row['id']
    raise SystemExit(f'agent-desktop does not list {title}: {json.dumps(rows)[:400]}')


def main():
    p = tools.paths()
    deskcrew = shutil.which('deskcrew')
    version = subprocess.run([deskcrew, '--version'], capture_output=True, text=True).stdout.strip()
    opened, targets = [], []
    try:
        open_targets(opened, targets)
    except BaseException:
        for hwnd in opened:
            user32.PostMessageW(hwnd, 0x0010, 0, 0)
        raise
    measure(deskcrew, version, p, opened, targets)


def open_targets(opened, targets):
    for name, title, cls, command, app in TARGETS:
        hwnd = find(title, cls)
        if hwnd is None:
            subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            hwnd = wait_for(title, cls)
            opened.append(hwnd)
        targets.append((name, title, hwnd, app))


def measure(deskcrew, version, p, opened, targets):
    results = {'deskcrew': version, 'when': time.strftime('%Y-%m-%d'), 'windows': {}, 'learn': {}}
    servers = {
        'mcp-windows': mcpclient.Server(p['mcp-windows'], []),
        'windows-mcp': mcpclient.Server('uvx', ['--python', '3.14', tools.WINDOWS_MCP, 'serve']),
    }
    try:
        results['learn'] = {
            'deskcrew': len(cli(deskcrew, ['help'])()),
            'agent-desktop': len(cli(p['agent-desktop'], ['--help'])()),
            'agent-cu': len(open(tools.skill_text('agent-cu'), encoding='utf-8').read()),
            'mcp-windows': len(json.dumps(servers['mcp-windows'].tools())),
            'windows-mcp': len(json.dumps(servers['windows-mcp'].tools())),
        }
        for name, title, hwnd, app in targets:
            ad_id = agent_desktop_id(p['agent-desktop'], hwnd, title)
            reads = {
                'deskcrew': cli(deskcrew, ['--agent', 'bench', 'see', title, '-f']),
                'agent-desktop': cli(p['agent-desktop'], ['snapshot', '--window-id', ad_id, '-i', '--compact']),
                'agent-cu': cli(p['agent-cu'], ['snapshot', '--app', title, '-i', '-c']),
                'mcp-windows': lambda: servers['mcp-windows'].call('ui_snapshot', {'windowHandle': str(hwnd)}),
                'windows-mcp': lambda: servers['windows-mcp'].call('Snapshot', {'use_vision': False}),
            }
            row = {}
            for tool, run in reads.items():
                samples = [timed(run) for _ in range(READS)]
                row[tool] = {'chars': [len(t) for t, _ in samples], 'ms': [round(ms) for _, ms in samples],
                             'sample': samples[-1][0][:300]}
                print(f'{name:14} {tool:14} chars {row[tool]["chars"]}  ms {row[tool]["ms"]}', flush=True)
            results['windows'][name] = row
    finally:
        for s in servers.values():
            s.close()
        subprocess.run([deskcrew, '--agent', 'bench', 'quit'], capture_output=True)
        for hwnd in opened:
            user32.PostMessageW(hwnd, 0x0010, 0, 0)  # WM_CLOSE
    json.dump(results, open(os.path.join(HERE, 'reading.json'), 'w', encoding='utf-8'), indent=1, ensure_ascii=False)


if __name__ == '__main__':
    main()
