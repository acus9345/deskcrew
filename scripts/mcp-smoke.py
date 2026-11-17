"""Start `deskcrew mcp`, shake hands, list the tools and make one call that needs no desktop.

    python scripts/mcp-smoke.py path/to/deskcrew
"""
import json, os, subprocess, sys

server = subprocess.Popen([os.path.abspath(sys.argv[1]), 'mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding='utf-8')
asked = 0


def ask(method, params=None):
    global asked
    asked += 1
    server.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': asked, 'method': method, 'params': params or {}}) + '\n')
    server.stdin.flush()
    reply = json.loads(server.stdout.readline())
    assert reply.get('id') == asked, reply
    return reply


hello = ask('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'smoke', 'version': '1'}})
assert hello['result']['serverInfo']['name'] == 'deskcrew', hello
names = [t['name'] for t in ask('tools/list')['result']['tools']]
assert names == ['see', 'do', 'shot', 'deskcrew'], names
version = ask('tools/call', {'name': 'deskcrew', 'arguments': {'args': ['--version']}})['result']
assert not version['isError'] and version['content'][0]['text'].startswith('deskcrew '), version
missing = ask('tools/call', {'name': 'do', 'arguments': {'steps': 'click OK'}})['result']
assert missing['isError'], missing
assert 'error' in ask('no/such/method')
server.stdin.close()
server.wait(10)
print('mcp ok:', ', '.join(names), '-', version['content'][0]['text'])
