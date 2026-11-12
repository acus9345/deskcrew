"""Record the demo video: three agents at once in throwaway windows, each with its own cursor.

    python demo/record.py [--deskcrew path] [--out demo/deskcrew-demo.mp4]
    python demo/record.py --hidden        # rehearse on a hidden desktop, no recording

Only the part of the screen the three windows cover is recorded, without the real mouse
pointer. It takes over that part of the screen for about 20 seconds: run it while you are away.
Needs ffmpeg (on PATH, or `pip install imageio-ffmpeg`).
"""
import argparse, os, shutil, subprocess, sys, tempfile, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(REPO, 'bench'))

# Where everything goes, in screen pixels: the recorded region and each window inside it.
REGION = (320, 200, 1920, 1080)
SKETCH = (340, 220, 900, 1040)
ORDER = (1260, 220, 960, 700)
NOTE = (1260, 940, 960, 320)
SECONDS = 18

# In pixels of a full-size shot of the Sketch window, title bar included.
HOUSE = '; '.join([
    '200,880 200,530 600,530 600,880 200,880',       # walls
    '170,540 400,320 630,540',                        # roof
    '360,880 360,720 450,720 450,880',                # door
    '250,600 320,600 320,670 250,670 250,600',        # window
    '760,200 810,220 830,270 810,320 760,340 710,320 690,270 710,220 760,200',  # sun
])
# Each agent's commands, run one after another; the three agents run at the same time.
STEPS = {
    'Claude Code': [['do', "set Name 'Ada Lovelace'; set Email ada@example.com; set Quantity 3; select Country Japan; "
                           "click Express; check 'Gift wrap'; click 'Submit order'; click Yes", '-w', 'Bench Order']],
    'Codex': [['shot', 'Sketch', '--edge', '0'], ['draw', HOUSE, '-w', 'Sketch']],
    'Cursor': [['type', 'Note', 'Shopping list: apples, bread, tea. Three agents, one desktop.', '-w', 'Note']],
}


def close_leftovers(titles):
    """Ask every window with one of these titles to close, and wait until they have."""
    if sys.platform != 'win32':
        return
    import ctypes
    user = ctypes.windll.user32
    for _ in range(50):
        found = [h for h in (user.FindWindowW(None, title) for title in titles) if h]
        if not found:
            return
        for h in found:
            user.PostMessageW(h, 0x10, 0, 0)
        time.sleep(0.2)


def ffmpeg():
    found = shutil.which('ffmpeg')
    if found:
        return found
    import imageio_ffmpeg
    return imageio_ffmpeg.get_ffmpeg_exe()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--deskcrew', default=shutil.which('deskcrew') or 'deskcrew')
    p.add_argument('--out', default=os.path.join(HERE, 'deskcrew-demo.mp4'))
    p.add_argument('--hidden', action='store_true', help='rehearse on a hidden desktop without recording')
    args = p.parse_args()
    exe = args.deskcrew
    work = tempfile.mkdtemp(prefix='deskcrew-demo-')
    log = os.path.join(work, 'order.log')

    if args.hidden:
        import desk
        place = desk.Desktop('deskcrewdemo')
        start = lambda line: place.start(line)
        def run(words):
            out = os.path.join(work, words[0] + '-' + words[-1].replace(' ', '-')[:40] + '.txt')
            place.run(subprocess.list2cmdline([exe] + words), out=out, timeout=120)
            return open(out, encoding='utf-8', errors='replace').read()
    else:
        start = lambda line: subprocess.Popen(line)
        run = lambda words: subprocess.run([exe] + words, capture_output=True, text=True, encoding='utf-8').stdout

    ps = 'powershell.exe -NoProfile -ExecutionPolicy Bypass -STA -File'
    # Any window left from an earlier run would share a title with the new ones.
    close_leftovers(['Sketch', 'Note', 'Bench Order', 'Backdrop'])
    # The backdrop goes first, so the demo windows open on top of it and nothing else shows.
    windows = [start(f'{ps} "{os.path.join(HERE, "windows.ps1")}" -Kind backdrop')]
    time.sleep(3)
    run(['place', '{},{},{},{}'.format(*REGION), '-w', 'Backdrop', '--agent', 'setup'])
    windows += [
        start(f'{ps} "{os.path.join(HERE, "windows.ps1")}" -Kind sketch'),
        start(f'{ps} "{os.path.join(HERE, "windows.ps1")}" -Kind notes'),
        start(f'{ps} "{os.path.join(REPO, "bench", "app.ps1")}" -Task order -Log "{log}" -Seed 3'),
    ]
    time.sleep(4)
    for title, (x, y, w, h) in [('Sketch', SKETCH), ('Bench Order', ORDER), ('Note', NOTE)]:
        run(['place', f'{x},{y},{w},{h}', '-w', title, '--agent', 'setup'])
    time.sleep(1)

    recorder = None
    if not args.hidden:
        x, y, w, h = REGION
        recorder = subprocess.Popen([ffmpeg(), '-y', '-loglevel', 'error', '-f', 'gdigrab', '-framerate', '30', '-draw_mouse', '0',
                                     '-offset_x', str(x), '-offset_y', str(y), '-video_size', f'{w}x{h}', '-i', 'desktop',
                                     '-t', str(SECONDS), '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-crf', '20', args.out])
        time.sleep(1.5)

    agents = []
    def act(agent, commands):
        size = None
        for words in commands:
            if words[0] == 'draw' and size:
                # Drawn for a 900x1040 window: fit it to the window the shot showed.
                k = min(size[0] / SKETCH[2], size[1] / SKETCH[3])
                fit = lambda point: ','.join(str(int(int(n) * k)) for n in point.split(','))
                words = ['draw', '; '.join(' '.join(fit(p) for p in stroke.split()) for stroke in words[1].split('; '))] + words[2:]
            reply = run(words + ['--agent', agent])
            if words[0] == 'shot' and reply:
                size = tuple(int(n) for n in reply.split()[-3].split('x'))

    for agent, commands in STEPS.items():
        t = threading.Thread(target=act, args=(agent, commands))
        t.start()
        agents.append(t)
        time.sleep(0.4)
    for t in agents:
        t.join()

    if recorder:
        recorder.wait(SECONDS + 10)
    for window in windows:
        window.kill() if hasattr(window, 'kill') else None
    run(['quit'])
    print('order log:', open(log, encoding='utf-8-sig').read() if os.path.exists(log) else 'none')
    if args.hidden:
        for name in os.listdir(work):
            if name.endswith('.txt'):
                print('---', name)
                print(open(os.path.join(work, name), encoding='utf-8', errors='replace').read())
    else:
        print('recorded', args.out)


if __name__ == '__main__':
    main()
