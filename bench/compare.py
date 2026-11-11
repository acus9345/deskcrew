"""Rerun the homepage comparison: the order form, deskcrew against the next-best tool, median of 3.

    python bench/compare.py                          # deskcrew and mcp-windows, 3 runs each
    python bench/compare.py --against windows-mcp --runs 5
    python bench/compare.py --report clean           # print a table from runs already made
    python bench/compare.py --report clean --save    # and write it to bench/published/

mcp-windows, Windows-MCP and agent-cu send real input, so they run on the desktop you see:
pass --visible, and only start it when you will be away from the computer for its length.
deskcrew runs on a hidden desktop unless --visible is given too, so both run the same way.
"""
import argparse, datetime, json, os, statistics, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import tools  # noqa: E402
from report import streamed  # noqa: E402

RESULTS = os.path.join(HERE, 'results.jsonl')
PUBLISHED = os.path.join(HERE, 'published')


def runs_for(label, task):
    rows = [json.loads(l) for l in open(RESULTS, encoding='utf-8') if l.strip()]
    return [r for r in rows if r['label'] == label and r['task'] == task]


def summary(runs):
    tokens = [streamed(r) for r in runs]
    med = lambda xs: statistics.median(xs) if xs else 0
    return {
        'right': sum(r['ok'] for r in runs), 'runs': len(runs),
        'time': med([r['wall_s'] for r in runs]), 'calls': med([r['calls'] for r in runs]),
        'read': med([t[0] for t in tokens]), 'written': med([t[1] for t in tokens]),
        'cost': med([r['cost_usd'] for r in runs]),
    }


def table(label, task):
    runs = runs_for(label, task)
    by_tool = {}
    for r in runs:
        by_tool.setdefault(r['tool'], []).append(r)
    if not by_tool:
        raise SystemExit(f'no runs labelled {label!r} for the {task} task in {RESULTS}')
    sums = {tool: summary(rs) for tool, rs in by_tool.items()}
    first = runs[0]
    lines = [f"{task} task, label `{label}`: {first['model']} at {first['effort']} effort, median of each tool's runs.", '',
             '| Tool | Right | Time | Model calls | Tokens read | Tokens written | Cost |',
             '| --- | --- | --- | --- | --- | --- | --- |']
    order = sorted(sums, key=lambda t: (t != 'deskcrew', sums[t]['time']))
    for tool in order:
        s = sums[tool]
        lines.append(f"| {tool} | {s['right']} of {s['runs']} | {s['time']:.0f} s | {s['calls']:g} | {s['read']:,.0f} | {s['written']:,.0f} | ${s['cost']:.3f} |")
    mine = sums.get('deskcrew')
    if mine:
        lines.append('')
        for tool in order:
            other = sums[tool]
            if tool == 'deskcrew' or not other['right'] or not mine['time'] or not mine['read']:
                continue
            lines.append(f"deskcrew against {tool}: {other['time'] / mine['time']:.1f}x faster, "
                         f"{other['read'] / mine['read']:.1f}x fewer tokens read, {other['cost'] / mine['cost']:.1f}x cheaper.")
    return '\n'.join(lines), runs


def save(label, task, text, runs):
    os.makedirs(PUBLISHED, exist_ok=True)
    base = os.path.join(PUBLISHED, f'{label}-{task}')
    with open(base + '.md', 'w', encoding='utf-8', newline='\n') as f:
        f.write(text + '\n')
    # Tool replies are left out: on the visible desktop they can hold the person's own window titles.
    with open(base + '.jsonl', 'w', encoding='utf-8', newline='\n') as f:
        for r in runs:
            slim = {k: v for k, v in r.items() if k not in ('tool_log', 'work', 'skills', 'answer')}
            slim['tokens_read'], slim['tokens_written'] = streamed(r)
            slim['commands'] = [u['command'] for u in r.get('tool_log', [])]
            f.write(json.dumps(slim) + '\n')
    print(f'wrote {base}.md and {base}.jsonl')


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--against', default='mcp-windows', help='comma-separated tools to compare with')
    p.add_argument('--task', default='order', choices=['order', 'stock'])
    p.add_argument('--runs', type=int, default=3)
    p.add_argument('--model', default='sonnet')
    p.add_argument('--label', default='compare-' + datetime.date.today().isoformat())
    p.add_argument('--visible', action='store_true', help="run on your own desktop: only while you are away")
    p.add_argument('--report', metavar='LABEL', help='only print the table for runs already made')
    p.add_argument('--save', action='store_true', help='write the table and the runs to bench/published/')
    args = p.parse_args()
    label = args.report or args.label
    if not args.report:
        others = [t for t in args.against.split(',') if t]
        if any(t in tools.NEEDS_REAL_DESKTOP for t in others) and not args.visible:
            raise SystemExit(f'{", ".join(t for t in others if t in tools.NEEDS_REAL_DESKTOP)} send real input: '
                             'pass --visible, and only while you are away from the computer')
        for tool in ['deskcrew'] + others:
            cmd = [sys.executable, os.path.join(HERE, 'run.py'), '--tool', tool, '--task', args.task,
                   '--runs', str(args.runs), '--model', args.model, '--label', label]
            if args.visible:
                cmd.append('--visible')
            if subprocess.run(cmd).returncode:
                raise SystemExit(f'{tool} runs stopped')
    text, runs = table(label, args.task)
    print(text)
    if args.save:
        save(label, args.task, text, runs)


if __name__ == '__main__':
    main()
