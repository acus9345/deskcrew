"""`1 + 2 =` on Calculator, knowing the result of every press: what each tool makes an agent read.

    python bench/calc_presses.py

deskcrew reads the window once, then each press replies with what changed. agent-desktop has no
reply of that kind, so it snapshots the window after each press, the way its skill says to check.
Opens Calculator if it is not open, and closes it again if it opened it.
"""
import json, os, shutil, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import reading, tools  # noqa: E402

PRESSES = ['One', 'Plus', 'Two', 'Equals']


def run(command):
    out = subprocess.run(command, capture_output=True, text=True, encoding='utf-8', errors='replace')
    return out.stdout + out.stderr


def refs(tree, found):
    if isinstance(tree, dict):
        if tree.get('ref_id') and tree.get('name'):
            found.setdefault(tree['name'], tree['ref_id'])
        for child in tree.get('children', []):
            refs(child, found)
    return found


def main():
    opened = reading.find('Calculator', 'ApplicationFrameWindow') is None
    if opened:
        subprocess.Popen(['cmd', '/c', 'start', 'calculator:'])
    hwnd = reading.wait_for('Calculator', 'ApplicationFrameWindow')
    deskcrew, desktop = shutil.which('deskcrew'), tools.paths()['agent-desktop']
    try:
        run([deskcrew, '--agent', 'calc', 'click', 'Clear', '-w', 'Calculator'])
        mine = [run([deskcrew, '--agent', 'calc', 'see', 'Calculator', '-f'])]
        mine += [run([deskcrew, '--agent', 'calc', 'click', name]) for name in PRESSES]
        shown = mine[-1]
        run([deskcrew, '--agent', 'calc', 'click', 'Clear'])
        window = reading.agent_desktop_id(desktop, hwnd, 'Calculator')
        snap = lambda: run([desktop, 'snapshot', '--window-id', window, '-i', '--compact'])
        theirs = [snap()]
        for name in PRESSES:
            ref = refs(json.loads(theirs[-1])['data']['tree'], {})[name]
            theirs.append(run([desktop, 'click', ref]))
            time.sleep(0.2)
            theirs.append(snap())
        result = {'deskcrew': sum(map(len, mine)), 'agent-desktop': sum(map(len, theirs)),
                  'deskcrew_last_reply': shown.strip(), 'agent_desktop_shows_3': '"Display is 3"' in theirs[-1]}
        print(json.dumps(result, indent=1))
        json.dump(result, open(os.path.join(HERE, 'calc_presses.json'), 'w', encoding='utf-8'), indent=1)
    finally:
        run([deskcrew, '--agent', 'calc', 'quit'])
        if opened:
            reading.user32.PostMessageW(hwnd, 0x0010, 0, 0)


if __name__ == '__main__':
    main()
