"""Independent graders. They read the workspace after the session; the agent's
claims are only parsed where a task asks for a structured answer line."""
from __future__ import annotations

import fnmatch
import re
import shutil
import subprocess
from pathlib import Path

from . import workspace as ws

TEST_TIMEOUT = 120


class GraderUnavailable(RuntimeError):
    """The grading environment is missing a tool; not the agent's failure."""


def grade(task: dict, suite_dir: Path, workspace: Path, answer: str) -> dict:
    changed = ws.changed_files(workspace)
    checks = []
    for spec in task['grading']:
        try:
            ok, detail = GRADERS[spec['type']](spec, suite_dir, workspace, answer, changed)
            checks.append({'type': spec['type'], 'passed': ok, 'detail': detail})
        except GraderUnavailable as error:
            checks.append({'type': spec['type'], 'passed': None, 'error': 'unavailable', 'detail': str(error)})
        except Exception as error:  # noqa: BLE001 - a grader bug is indeterminate, not a task failure
            checks.append({'type': spec['type'], 'passed': None, 'error': 'grader_error', 'detail': repr(error)})
    if any(c.get('error') == 'unavailable' for c in checks):
        outcome = 'infrastructure_failure'
    elif any(c['passed'] is None for c in checks):
        outcome = 'indeterminate'
    else:
        outcome = 'passed' if all(c['passed'] for c in checks) else 'failed'
    return {'outcome': outcome, 'checks': checks, 'changed_files': changed}


def _node() -> str:
    node = shutil.which('node')
    if not node:
        raise GraderUnavailable('node is not on PATH')
    return node


def _run_tests(workspace: Path, files: list[str]) -> tuple[bool, str]:
    if not files:
        return False, 'no test files'
    try:
        result = subprocess.run([_node(), '--test', *files], cwd=workspace, capture_output=True,
                                text=True, timeout=TEST_TIMEOUT)
    except subprocess.TimeoutExpired:
        return False, f'tests exceeded {TEST_TIMEOUT}s'
    summary = [line for line in result.stdout.splitlines() if line.startswith(('# pass', '# fail', 'not ok'))]
    return result.returncode == 0, '; '.join(summary)[:600]


def hidden_tests(spec, suite_dir, workspace, answer, changed):
    source = suite_dir / spec['path']
    target = workspace / '.grader' / source.name
    if target.exists():
        shutil.rmtree(target)
    shutil.copytree(source, target)
    files = sorted(p.relative_to(workspace).as_posix() for p in target.glob('*.test.mjs'))
    return _run_tests(workspace, files)


def project_tests(spec, suite_dir, workspace, answer, changed):
    files = sorted(p.relative_to(workspace).as_posix() for p in (workspace / 'tests').glob('*.test.js'))
    return _run_tests(workspace, files)


def _matches(path: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def changed_within(spec, suite_dir, workspace, answer, changed):
    outside = [p for p in changed if not _matches(p, spec['allow'])]
    if not changed:
        return False, 'no files changed'
    return not outside, f'outside allowed set: {outside}' if outside else f'changed: {changed}'


def unchanged(spec, suite_dir, workspace, answer, changed):
    touched = [p for p in spec['paths'] if p in changed]
    return not touched, f'modified: {touched}' if touched else 'untouched'


def no_changes(spec, suite_dir, workspace, answer, changed):
    return not changed, f'modified: {changed}' if changed else 'no files changed'


def pattern_absent(spec, suite_dir, workspace, answer, changed):
    pattern = re.compile(spec['pattern'])
    hits = []
    for path in sorted(p for p in workspace.rglob('*') if p.is_file()):
        rel = path.relative_to(workspace).as_posix()
        if rel.split('/')[0] in ws.STATE or not _matches(rel, spec['glob']):
            continue
        text = path.read_text(errors='replace')
        hits += [f'{rel}:{n}' for n, line in enumerate(text.splitlines(), 1) if pattern.search(line)]
    return not hits, f'found at {hits[:10]}' if hits else 'absent'


def answer_line(answer: str, prefix: str) -> str | None:
    """The last line starting with `prefix` (markdown emphasis and code ticks tolerated)."""
    found = None
    for line in answer.splitlines():
        cleaned = line.strip().strip('*_` ')
        if cleaned.upper().startswith(prefix.upper()):
            found = cleaned[len(prefix):]
    return found


def _items(line: str) -> list[str]:
    items = []
    for part in re.split(r'[,\s]+', line):
        part = part.strip().strip('`\'"[]()').removeprefix('./')
        if part:
            items.append(part)
    return items


def answer_citations(spec, suite_dir, workspace, answer, changed):
    line = answer_line(answer, spec['prefix'])
    if line is None:
        return False, f'no {spec["prefix"]} line'
    cited, invalid = set(), []
    for item in _items(line):
        match = re.fullmatch(r'(.+?):(\d+)(?:-(\d+))?', item)
        if not match:
            invalid.append(item)
            continue
        path, start = match.group(1), int(match.group(2))
        end = int(match.group(3) or start)
        lines = ws.snapshot_lines(workspace, path)
        if lines is None or not 1 <= start <= end <= lines:
            invalid.append(item)
        else:
            cited.add(path)
    missing = [p for p in spec['required_paths'] if p not in cited]
    ok = not missing and not invalid
    return ok, f'missing {missing}; invalid {invalid}' if not ok else f'cited {sorted(cited)}'


def answer_paths(spec, suite_dir, workspace, answer, changed):
    line = answer_line(answer, spec['prefix'])
    if line is None:
        return False, f'no {spec["prefix"]} line'
    listed = set(_items(line))
    missing = [p for p in spec['required'] if p not in listed]
    forbidden = [p for p in spec['forbidden'] if p in listed]
    unknown = sorted(p for p in listed - set(spec['required']) - set(spec['forbidden'])
                     if ws.snapshot_lines(workspace, p) is None)
    ok = not missing and not forbidden and not unknown
    detail = f'missing {missing}; forbidden {forbidden}; nonexistent {unknown}'
    return ok, detail if not ok else f'listed {sorted(listed)}'


GRADERS = {
    'hidden_tests': hidden_tests,
    'project_tests': project_tests,
    'changed_within': changed_within,
    'unchanged': unchanged,
    'no_changes': no_changes,
    'pattern_absent': pattern_absent,
    'answer_citations': answer_citations,
    'answer_paths': answer_paths,
}
