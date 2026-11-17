"""Sum up bench results: one row per tool (or label) and task, medians over the runs.

    python bench/report.py [--by tool|label] [--labels a,b] [--markdown]
"""
import argparse, json, os, statistics

HERE = os.path.dirname(os.path.abspath(__file__))


def streamed(r):
    """Tokens added up call by call from the saved stream. The final summary can undercount."""
    path = os.path.join(r.get('work', ''), 'stream.jsonl')
    if not os.path.exists(path):
        return r['input'] + r['cache_write'] + r['cache_read'], r['output']
    read = written = 0
    for line in open(path, encoding='utf-8', errors='replace'):
        try:
            m = json.loads(line)
        except ValueError:
            continue
        e = m.get('event', {}) if m.get('type') == 'stream_event' else {}
        if e.get('type') == 'message_start':
            u = e['message'].get('usage', {})
            read += sum(u.get(k) or 0 for k in ('input_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'))
        elif e.get('type') == 'message_delta':
            written += (e.get('usage') or {}).get('output_tokens') or 0
    return read, written


def median(values):
    return statistics.median(values) if values else 0


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--file', default=os.path.join(HERE, 'results.jsonl'))
    p.add_argument('--by', default='label', choices=['label', 'tool'])
    p.add_argument('--labels', help='only these labels, comma-separated')
    p.add_argument('--markdown', action='store_true')
    args = p.parse_args()
    rows = [json.loads(l) for l in open(args.file, encoding='utf-8') if l.strip()]
    if args.labels:
        keep = args.labels.split(',')
        rows = [r for r in rows if r['label'] in keep]
    groups = {}
    for r in rows:
        groups.setdefault((r[args.by], r['task']), []).append(r)
    head = ['', 'task', 'right', 'time (s)', 'model calls', 'tool calls', 'tokens read', 'tokens written', 'thinking', 'cost (USD)']
    out = []
    for (key, task), runs in groups.items():
        tokens = [streamed(r) for r in runs]
        read = [t[0] for t in tokens]
        out.append([key, task, f"{sum(r['ok'] for r in runs)}/{len(runs)}",
                     f"{median([r['wall_s'] for r in runs]):.1f}", f"{median([r['calls'] for r in runs]):g}",
                     f"{median([r['tool_calls'] for r in runs]):g}", f"{median(read):,.0f}",
                     f"{median([t[1] for t in tokens]):,.0f}", f"{median([r['thinking_tokens'] for r in runs]):,.0f}",
                     f"{median([r['cost_usd'] for r in runs]):.3f}"])
    if args.markdown:
        print('| ' + ' | '.join(head) + ' |')
        print('|' + ' --- |' * len(head))
        for row in out:
            print('| ' + ' | '.join(row) + ' |')
    else:
        widths = [max(len(str(x[i])) for x in [head] + out) for i in range(len(head))]
        for row in [head] + out:
            print('  '.join(str(c).ljust(w) for c, w in zip(row, widths)))


if __name__ == '__main__':
    main()
