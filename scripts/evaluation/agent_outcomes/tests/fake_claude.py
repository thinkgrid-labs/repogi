#!/usr/bin/env python3
"""Stand-in for the Claude Code CLI in harness tests. Calls no model.

Behaviour comes from FAKE_CLAUDE_MODE: `fix` edits src/money/format.js like the
reference solution, `sleep` hangs (timeout test), `crash` exits non-zero
without a result event. It records its argv to FAKE_CLAUDE_ARGV.
"""
import json
import os
import sys
import time
from pathlib import Path

if sys.argv[1:] == ['--version']:
    print('0.0.0-fake (Claude Code)')
    sys.exit(0)

Path(os.environ['FAKE_CLAUDE_ARGV']).write_text(json.dumps(sys.argv[1:]))
mode = os.environ.get('FAKE_CLAUDE_MODE', 'fix')
mcp = json.loads(Path(sys.argv[sys.argv.index('--mcp-config') + 1]).read_text())
servers = [{'name': name, 'status': 'connected'} for name in mcp['mcpServers']]
emit = lambda event: print(json.dumps(event), flush=True)  # noqa: E731
emit({'type': 'system', 'subtype': 'init', 'model': 'fake-model', 'claude_code_version': '0.0.0-fake',
      'tools': ['Read', 'Edit'], 'mcp_servers': servers})
if mode == 'sleep':
    time.sleep(60)
if mode == 'crash':
    sys.exit(3)
emit({'type': 'assistant', 'message': {'id': 'm1', 'content': [
    {'type': 'tool_use', 'name': 'Read', 'input': {'file_path': 'src/money/format.js'}},
    {'type': 'tool_use', 'name': 'Grep', 'input': {'pattern': 'formatPrice'}}]}})
if mcp['mcpServers']:
    emit({'type': 'assistant', 'message': {'id': 'm2', 'content': [
        {'type': 'tool_use', 'name': 'mcp__repogi__atlas_context', 'input': {'task': 'price format'}}]}})
target = Path('src/money/format.js')
text = target.read_text()
target.write_text(text.replace(
    "  const whole = Math.trunc(cents / 100);\n  const fraction = String(Math.abs(cents % 100)).padStart(2, '0');\n"
    "  return `${symbol}${whole}.${fraction}`;",
    "  const sign = cents < 0 ? '-' : '';\n  const whole = Math.trunc(Math.abs(cents) / 100);\n"
    "  const fraction = String(Math.abs(cents % 100)).padStart(2, '0');\n  return `${sign}${symbol}${whole}.${fraction}`;"))
emit({'type': 'assistant', 'message': {'id': 'm3', 'content': [{'type': 'text', 'text': 'Fixed.'}]}})
emit({'type': 'result', 'subtype': 'success', 'is_error': False, 'result': 'Fixed the sign.', 'num_turns': 3,
      'duration_ms': 1200, 'total_cost_usd': 0.0123,
      'usage': {'input_tokens': 100, 'output_tokens': 50, 'cache_creation_input_tokens': 10,
                'cache_read_input_tokens': 2000}})
