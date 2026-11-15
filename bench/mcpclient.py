"""A minimal MCP client over stdio, enough to list a server's tools and call one."""
import json, subprocess, threading, queue


class Server:
    def __init__(self, command, args, cwd=None):
        self.proc = subprocess.Popen([command, *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                     cwd=cwd, text=True, encoding='utf-8', bufsize=1)
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.next_id = 0
        self.request('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'bench', 'version': '1'}}, timeout=120)
        self.notify('notifications/initialized')

    def _read(self):
        for line in self.proc.stdout:
            self.lines.put(line)

    def notify(self, method, params=None):
        self.proc.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': method, **({'params': params} if params else {})}) + '\n')
        self.proc.stdin.flush()

    def request(self, method, params=None, timeout=60):
        self.next_id += 1
        wanted = self.next_id
        self.proc.stdin.write(json.dumps({'jsonrpc': '2.0', 'id': wanted, 'method': method, 'params': params or {}}) + '\n')
        self.proc.stdin.flush()
        while True:
            line = self.lines.get(timeout=timeout)
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get('id') == wanted:
                if 'error' in message:
                    raise RuntimeError(message['error'])
                return message['result']

    def tools(self):
        return self.request('tools/list')['tools']

    def call(self, name, arguments, timeout=120):
        result = self.request('tools/call', {'name': name, 'arguments': arguments}, timeout=timeout)
        return ''.join(part.get('text', '') for part in result.get('content', []) if part.get('type') == 'text')

    def close(self):
        try:
            self.proc.kill()
        except OSError:
            pass
