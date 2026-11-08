"""Give a headless coding agent the same desktop task several times and measure what it costs:
wall time, model calls, thinking, tokens, actions per call, and whether the result is right.

Everything runs on a hidden Windows desktop, so the agent, the app and the tool under test can
never reach the person's screen, focus, mouse or keyboard.

    python bench/run.py --tool deskcrew --task order --runs 3 --label baseline
"""
import argparse, json, os, re, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
WORK_ROOT = 'C:/Users/Public/deskcrew-bench'
sys.path.insert(0, HERE)
from desk import Desktop
import tools

TASKS = {
    'order': ("On this computer there is a window titled 'Bench Order'. Use it to place an order for Ada Lovelace, "
              "email ada@example.com: 3 items, shipped Express to Japan, gift wrapped. Confirm the order when asked, "
              "then tell me the order number."),
    'stock': ("On this computer there is a window titled 'Bench Stock' with a table of items. Find the item with the "
              "lowest stock, type its exact name into the Answer box, press Check, and tell me what the result says."),
}
ACTIONS = {'click', 'set', 'type', 'key', 'select', 'check', 'uncheck', 'toggle', 'expand', 'collapse', 'clear',
           'scroll', 'drag', 'draw', 'hover', 'pick-file', 'down', 'up'}


def clean_env(extra_path, config=None, keep_deskcrew=True):
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith(('CLAUDE', 'ANTHROPIC', 'ENTRYPOINT'))}
    path = env['PATH'].split(os.pathsep)
    if not keep_deskcrew:  # another tool's agent should not stumble on an installed deskcrew
        path = [p for p in path if not os.path.exists(os.path.join(p, 'deskcrew.exe'))]
    env['PATH'] = os.pathsep.join(([extra_path] if extra_path else []) + path)
    if config:
        env['CLAUDE_CONFIG_DIR'] = config
    return env


def fresh_config(work):
    """A config folder with only the sign-in, so the person's own CLAUDE.md, skills, plugins and
    MCP servers stay out of the run. It is deleted after the run."""
    config = os.path.join(work, 'config')
    os.makedirs(config)
    shutil.copy(os.path.expanduser('~/.claude/.credentials.json'), os.path.join(config, '.credentials.json'))
    return config


def tail(path, proc, timeout):
    """Read the agent's stream as it is written, stamping each line with when it arrived."""
    lines, pos, buf, start = [], 0, '', time.time()
    while True:
        alive = proc.alive()
        if os.path.exists(path):
            with open(path, 'rb') as f:
                f.seek(pos); chunk = f.read(); pos += len(chunk)
            buf += chunk.decode('utf-8', errors='replace')
            *done, buf = buf.split('\n')
            now = time.time() - start
            lines += [(now, l) for l in done if l.strip()]
        if not alive:
            break
        if time.time() - start > timeout:
            proc.kill(); lines.append((time.time() - start, json.dumps({'type': 'timeout'})))
            break
        time.sleep(0.02)
    return lines, time.time() - start


def deskcrew_actions(command):
    """How many UI actions one shell command asks deskcrew for, counting the steps of a `do`."""
    count = 0
    for m in re.finditer(r'deskcrew(?:\.exe)?\s+(?:--agent\s+\S+\s+)?([a-z-]+)(.*?)(?=deskcrew|$)', command, re.S):
        sub, rest = m.group(1), m.group(2)
        if sub == 'do':
            q = re.search(r'"([^"]*)"|\'([^\']*)\'', rest)
            steps = (q.group(1) or q.group(2)) if q else ''
            count += sum(1 for s in steps.split(';') if s.strip().split(' ')[0] in ACTIONS)
        elif sub in ACTIONS:
            count += 1
    return count


def analyse(lines, wall):
    calls, tools_used, cur, last_mark = [], [], None, None
    result, init = {}, {}
    pending = {}
    streamed = {}
    for t, raw in lines:
        try:
            m = json.loads(raw)
        except ValueError:
            continue
        kind = m.get('type')
        if kind == 'system' and m.get('subtype') == 'init':
            init = m; last_mark = t
        elif kind == 'stream_event':
            e = m['event']; et = e.get('type')
            if et == 'message_start':
                u = e.get('message', {}).get('usage') or {}
                for k in ('input_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'):
                    streamed[k] = streamed.get(k, 0) + (u.get(k) or 0)
            if et == 'message_delta':
                streamed['output_tokens'] = streamed.get('output_tokens', 0) + ((e.get('usage') or {}).get('output_tokens') or 0)
            if et == 'message_start':
                cur = {'sent': last_mark if last_mark is not None else 0, 'first': t, 'blocks': [], 'thinking': 0.0}
            elif et == 'content_block_start' and cur is not None:
                cur['blocks'].append([e['content_block']['type'], t, None])
            elif et == 'content_block_stop' and cur is not None and cur['blocks']:
                cur['blocks'][-1][2] = t
            elif et == 'message_stop' and cur is not None:
                cur['end'] = t; calls.append(cur); cur = None
        elif kind == 'assistant':
            for b in m['message'].get('content', []):
                if b.get('type') == 'tool_use':
                    cmd = b['input'].get('command') if isinstance(b.get('input'), dict) else None
                    pending[b['id']] = {'name': b['name'], 'command': cmd or json.dumps(b.get('input'))[:400], 'start': t}
        elif kind == 'user':
            content = m.get('message', {}).get('content', [])
            for b in content if isinstance(content, list) else []:
                if b.get('type') == 'tool_result' and b.get('tool_use_id') in pending:
                    u = pending.pop(b['tool_use_id']); u['end'] = t
                    out = b.get('content')
                    out = out if isinstance(out, str) else ''.join(x.get('text', '') for x in out or [] if isinstance(x, dict))
                    u['output_chars'] = len(out); u['output'] = out[:3000]
                    tools_used.append(u)
            last_mark = t
        elif kind == 'result':
            result = m
    usage = result.get('usage', {})
    if not usage.get('input_tokens') and not usage.get('cache_read_input_tokens'):
        usage = streamed  # the run was cut off before its summary: add up what each call reported
    think = sum((b[2] or c['end']) - b[1] for c in calls for b in c['blocks'] if b[0] == 'thinking')
    # Time from sending a request to the first tool call or answer: what the person waits per step.
    to_act = []
    for c in calls:
        acts = [b[1] for b in c['blocks'] if b[0] in ('tool_use', 'text')]
        to_act.append((acts[0] if acts else c['end']) - c['sent'])
    model_s = sum(c['end'] - c['sent'] for c in calls)
    tool_s = sum(u['end'] - u['start'] for u in tools_used)
    actions = sum(deskcrew_actions(u['command']) for u in tools_used if u['name'] == 'Bash')
    return {
        'wall_s': round(wall, 1), 'model_s': round(model_s, 1), 'tool_s': round(tool_s, 1),
        'think_s': round(think, 1), 'to_act_s': round(sum(to_act), 1),
        'calls': len(calls), 'tool_calls': len(tools_used), 'ui_actions': actions,
        'input': usage.get('input_tokens', 0), 'cache_write': usage.get('cache_creation_input_tokens', 0),
        'cache_read': usage.get('cache_read_input_tokens', 0), 'output': usage.get('output_tokens', 0),
        'thinking_tokens': (usage.get('output_tokens_details') or {}).get('thinking_tokens', 0),
        'cost_usd': round(result.get('total_cost_usd') or 0, 4), 'answer': result.get('result', ''),
        'error': result.get('is_error', True) or not result, 'tool_log': tools_used,
        'tools_offered': len(init.get('tools', [])), 'skills': init.get('skills'),
    }


def verify(task, log, answer):
    events = [json.loads(l) for l in open(log, encoding='utf-8-sig') if l.strip()] if os.path.exists(log) else []
    if task == 'order':
        orders = [e for e in events if e.get('event') == 'order']
        want = {'name': 'Ada Lovelace', 'email': 'ada@example.com', 'quantity': 3, 'country': 'Japan', 'shipping': 'Express', 'gift': True}
        if len(orders) != 1:
            return False, f'{len(orders)} orders placed'
        o = orders[0]
        wrong = [k for k, v in want.items() if o.get(k) != v]
        if wrong:
            return False, 'wrong ' + ', '.join(f'{k}={o.get(k)!r}' for k in wrong)
        if str(o['number']) not in answer:
            return False, 'order number not reported'
        return True, 'ok'
    checks = [e for e in events if e.get('event') == 'check']
    if not checks or not checks[-1].get('right'):
        return False, f'{len(checks)} checks, last {checks[-1] if checks else None}'
    if 'correct' not in answer.lower():
        return False, 'result not reported'
    return True, 'ok' if len(checks) == 1 else f'ok after {len(checks)} checks'


def one_run(args, desk, n):
    # Outside the person's profile: Claude Code reads CLAUDE.md from every folder above the work
    # folder, and the profile holds their own .claude/CLAUDE.md.
    os.makedirs(WORK_ROOT, exist_ok=True)
    work = tempfile.mkdtemp(prefix=f'{args.tool}-', dir=WORK_ROOT)
    setup = tools.prepare(args.tool, work, args.deskcrew)
    log = os.path.join(work, 'app.log')
    seed = 1000 + n
    app = desk.start(f'powershell.exe -NoProfile -ExecutionPolicy Bypass -STA -File "{HERE}\\app.ps1" -Task {args.task} -Log "{log}" -Seed {seed}')
    for _ in range(100):
        if os.path.exists(log) and 'ready' in open(log, encoding='utf-8-sig').read():
            break
        time.sleep(0.1)
    out = os.path.join(work, 'stream.jsonl')
    cmd = ['claude', '-p', TASKS[args.task], '--model', args.model, '--output-format', 'stream-json', '--verbose',
           '--include-partial-messages', '--setting-sources', 'project', '--strict-mcp-config',
           '--permission-mode', 'bypassPermissions', '--no-session-persistence'] + setup['flags']
    if args.effort:
        cmd += ['--effort', args.effort]
    exe = shutil.which('claude')
    line = subprocess.list2cmdline([exe] + cmd[1:])
    # A service of the same version may already run on this desktop: start from a fresh one.
    for stop in setup.get('cleanup', []):
        desk.run(stop, env=clean_env(setup.get('path')), timeout=30)
    config = fresh_config(work)
    try:
        proc = desk.start(line, cwd=work, env=clean_env(setup.get('path'), config, args.tool == 'deskcrew'), out=out)
        lines, wall = tail(out, proc, args.timeout)
    finally:
        shutil.rmtree(config, ignore_errors=True)
    time.sleep(0.5)
    app.kill()
    for stop in setup.get('cleanup', []):
        desk.run(stop, env=clean_env(setup.get('path')), timeout=30)
    r = analyse(lines, wall)
    r['ok'], r['why'] = verify(args.task, log, r['answer'])
    r.update(tool=args.tool, task=args.task, label=args.label, model=args.model, effort=args.effort or 'default', run=n, work=work, desktop=desk.name)
    return r


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--tool', default='deskcrew')
    p.add_argument('--task', default='order', choices=TASKS)
    p.add_argument('--runs', type=int, default=3)
    p.add_argument('--model', default='sonnet')
    p.add_argument('--effort')
    p.add_argument('--label', default='')
    p.add_argument('--deskcrew', default=os.path.join(HERE, '..', 'target', 'release', 'deskcrew.exe'))
    p.add_argument('--timeout', type=int, default=600)
    p.add_argument('--out', default=os.path.join(HERE, 'results.jsonl'))
    p.add_argument('--visible', action='store_true', help="run on the person's own desktop: only while they are away")
    args = p.parse_args()
    if tools.NEEDS_REAL_DESKTOP & {args.tool} and not args.visible:
        raise SystemExit(f'{args.tool} uses real input, which only reaches the visible desktop: pass --visible while the person is away')
    desk = Desktop('Default' if args.visible else 'deskcrewbench')
    for n in range(args.runs):
        r = one_run(args, desk, n)
        with open(args.out, 'a', encoding='utf-8') as f:
            f.write(json.dumps(r) + '\n')
        print(f"{args.tool} {args.task} #{n} ok={r['ok']} ({r['why']}) wall={r['wall_s']}s calls={r['calls']} "
              f"tools={r['tool_calls']} actions={r['ui_actions']} think={r['think_s']}s to_act={r['to_act_s']}s "
              f"in={r['input']}+{r['cache_write']}w+{r['cache_read']}r out={r['output']} (thinking {r['thinking_tokens']}) ${r['cost_usd']}",
              flush=True)
    desk.close()


if __name__ == '__main__':
    main()
