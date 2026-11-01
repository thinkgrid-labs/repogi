"""Task suites: loading, validation and content digests."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path

from . import HARNESS, ROOT

GRADERS = {
    'hidden_tests': {'path'},
    'project_tests': set(),
    'changed_within': {'allow'},
    'unchanged': {'paths'},
    'no_changes': set(),
    'pattern_absent': {'glob', 'pattern'},
    'answer_citations': {'prefix', 'required_paths'},
    'answer_paths': {'prefix', 'required', 'forbidden'},
}
CATEGORIES = {'bug_fix', 'feature', 'refactor', 'architecture', 'requirements', 'test_selection'}


class SuiteError(ValueError):
    pass


def tree_digest(root: Path) -> str:
    """SHA-256 over sorted relative paths and bytes; ignores caches."""
    digest = hashlib.sha256()
    for path in sorted(p for p in root.rglob('*') if p.is_file() and '__pycache__' not in p.parts):
        digest.update(path.relative_to(root).as_posix().encode() + b'\0')
        digest.update(hashlib.sha256(path.read_bytes()).digest())
    return digest.hexdigest()


def load(path: Path) -> dict:
    suite = json.loads(path.read_text())
    problems = validate(suite, path.parent)
    if problems:
        raise SuiteError('; '.join(problems))
    suite['_dir'] = str(path.parent)
    suite['_fixture'] = str(ROOT / suite['repository']['fixture'])
    return suite


def validate(suite: dict, base: Path) -> list[str]:
    problems = []
    if suite.get('schema') != 'repogi-agent-outcomes-suite/1':
        problems.append('unknown suite schema')
    fixture = ROOT / suite.get('repository', {}).get('fixture', '')
    if not fixture.is_dir():
        problems.append(f'fixture repository missing: {fixture}')
    seen = set()
    for task in suite.get('tasks', []):
        tid = task.get('id', '?')
        if tid in seen:
            problems.append(f'{tid}: duplicate id')
        seen.add(tid)
        if task.get('category') not in CATEGORIES:
            problems.append(f'{tid}: unknown category {task.get("category")!r}')
        if task.get('kind') not in ('change', 'question'):
            problems.append(f'{tid}: kind must be change or question')
        if not task.get('prompt'):
            problems.append(f'{tid}: empty prompt')
        if not task.get('grading'):
            problems.append(f'{tid}: no graders; grading must not rely on agent self-report')
        for grader in task.get('grading', []):
            required = GRADERS.get(grader.get('type'))
            if required is None:
                problems.append(f'{tid}: unknown grader {grader.get("type")!r}')
                continue
            missing = required - grader.keys()
            if missing:
                problems.append(f'{tid}: grader {grader["type"]} lacks {sorted(missing)}')
            if grader['type'] == 'hidden_tests' and not (base / grader.get('path', '')).is_dir():
                problems.append(f'{tid}: hidden tests missing at {grader.get("path")}')
        # Graders and reference solutions must never be inside the agent-visible repository.
        hidden = [g['path'] for g in task.get('grading', []) if g.get('type') == 'hidden_tests']
        for rel in hidden:
            if (base / rel).resolve().is_relative_to(fixture.resolve()):
                problems.append(f'{tid}: hidden grader inside the agent workspace')
        if task.get('prompt') and any(word in task['prompt'] for word in ('hidden', '.grader', 'reference/')):
            problems.append(f'{tid}: prompt mentions grader material')
    if not seen:
        problems.append('suite has no tasks')
    return problems


def digest(suite: dict) -> dict:
    """Content identity of everything that defines the tasks and their grading."""
    base = Path(suite['_dir'])
    public = {k: v for k, v in suite.items() if not k.startswith('_')}
    return {
        'suite_json': hashlib.sha256(json.dumps(public, sort_keys=True).encode()).hexdigest(),
        'fixture_tree': tree_digest(Path(suite['_fixture'])),
        'hidden_tree': tree_digest(base / 'hidden'),
        'harness_sources': harness_digest(),
    }


def harness_digest() -> str:
    """Sources whose change could alter a result: harness modules and the runner."""
    digest = hashlib.sha256()
    files = sorted((HARNESS / 'harness').glob('*.py')) + [HARNESS / 'run.py']
    for path in files:
        digest.update(path.name.encode() + b'\0' + path.read_bytes())
    return digest.hexdigest()


def select(suite: dict, ids: list[str] | None) -> list[dict]:
    tasks = suite['tasks']
    if not ids:
        return tasks
    unknown = set(ids) - {t['id'] for t in tasks}
    if unknown:
        raise SuiteError(f'unknown task ids: {sorted(unknown)}')
    return [t for t in tasks if t['id'] in ids]
