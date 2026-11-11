"""How each desktop tool is offered to the agent: the way its own setup or README says to.

Every agent gets the same built-in tools (Bash and Skill). A command-line tool comes with its
skill and the line its README asks for in the agent's instructions; an MCP server is added with
--mcp-config. Paths to the other tools come from bench/tools.json (see README.md here).
"""
import json, os, shutil, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, '..'))
VENDOR = os.path.join(HERE, 'vendor')
SKILL_URLS = {
    'agent-cu': 'https://raw.githubusercontent.com/kortix-ai/agent-computer-use/main/skills/agent-computer-use/SKILL.md',
}
# Real input (SendInput: the mouse, typed keys) reaches only the desktop the person is using, so
# these tools cannot run on the hidden one, and must not run while the person is at the computer.
NEEDS_REAL_DESKTOP = {'windows-mcp', 'mcp-windows', 'agent-cu'}


def paths():
    with open(os.path.join(HERE, 'tools.json'), encoding='utf-8') as f:
        return json.load(f)


def deskcrew_rule():
    """The standing rule `deskcrew setup` writes into an agent's instructions, read from setup.rs."""
    text = open(os.path.join(REPO, 'src', 'setup.rs'), encoding='utf-8').read()
    start = text.index('fn standing_rule()')
    body = text[text.index('"', start) + 1:]
    body = body[:body.index('"\n')].replace('\\"', '"')
    body = body.replace('{RULE_START}', '<!-- deskcrew: start -->').replace('{RULE_END}', '<!-- deskcrew: end -->')
    return body.replace('\\n', '\n')


def skill_text(tool, exe=None):
    os.makedirs(os.path.join(VENDOR, tool), exist_ok=True)
    cached = os.path.join(VENDOR, tool, 'SKILL.md')
    if not os.path.exists(cached):
        if tool == 'agent-desktop':
            import subprocess
            out = subprocess.run([exe, 'skills', 'get', 'agent-desktop'], capture_output=True, text=True, encoding='utf-8').stdout
            text = json.loads(out)['data']['content']
        else:
            text = urllib.request.urlopen(SKILL_URLS[tool]).read().decode('utf-8')
        open(cached, 'w', encoding='utf-8').write(text)
    return cached


def add_skill(work, name, source):
    folder = os.path.join(work, '.claude', 'skills', name)
    os.makedirs(folder)
    shutil.copy(source, os.path.join(folder, 'SKILL.md'))


def prepare(tool, work, deskcrew_exe):
    flags = ['--tools', 'Bash,Skill']
    bin_dir = os.path.join(work, 'bin'); os.makedirs(bin_dir)
    if tool == 'deskcrew':
        shutil.copy(deskcrew_exe, os.path.join(bin_dir, 'deskcrew.exe'))
        add_skill(work, 'deskcrew', os.path.join(REPO, 'skills', 'deskcrew', 'SKILL.md'))
        open(os.path.join(work, 'CLAUDE.md'), 'w', encoding='utf-8').write(deskcrew_rule() + '\n')
        return {'flags': flags, 'path': bin_dir, 'cleanup': [os.path.join(bin_dir, 'deskcrew.exe') + ' quit']}
    if tool == 'deskcrew-mcp':
        # The MCP server alone: no skill, no standing rule, no deskcrew on PATH.
        exe = os.path.join(work, 'mcp', 'deskcrew.exe'); os.makedirs(os.path.dirname(exe))
        shutil.copy(deskcrew_exe, exe)
        config = os.path.join(work, 'mcp.json')
        json.dump({'mcpServers': {'deskcrew': {'command': exe, 'args': ['mcp']}}}, open(config, 'w', encoding='utf-8'))
        return {'flags': flags + ['--mcp-config', config], 'path': bin_dir, 'cleanup': [exe + ' quit']}
    p = paths()
    if tool == 'agent-desktop':
        shutil.copy(p['agent-desktop'], os.path.join(bin_dir, 'agent-desktop.exe'))
        add_skill(work, 'agent-desktop', skill_text('agent-desktop', p['agent-desktop']))
        # The lines its README gives for an agent's instructions.
        open(os.path.join(work, 'CLAUDE.md'), 'w', encoding='utf-8').write(
            'Use `agent-desktop` to see and operate desktop apps. Before the first use, run\n'
            '`agent-desktop skills get desktop` and `agent-desktop skills get platform`, and follow them.\n')
        return {'flags': flags, 'path': bin_dir}
    if tool == 'agent-cu':
        shutil.copy(p['agent-cu'], os.path.join(bin_dir, 'agent-cu.exe'))
        add_skill(work, 'agent-computer-use', skill_text('agent-cu'))
        return {'flags': flags, 'path': bin_dir}
    servers = {
        'windows-mcp': {'command': 'uvx', 'args': ['--python', '3.14', 'windows-mcp==0.8.8', 'serve']},
        'mcp-windows': {'command': p.get('mcp-windows', ''), 'args': []},
    }
    if tool in servers:
        config = os.path.join(work, 'mcp.json')
        json.dump({'mcpServers': {tool: servers[tool]}}, open(config, 'w', encoding='utf-8'))
        return {'flags': flags + ['--mcp-config', config], 'path': bin_dir}
    raise SystemExit('unknown tool ' + tool)
