"""Paired coding-agent outcome benchmark: the same agent with and without Repogi.

Standard library only. Nothing here starts a billable model session unless a
reviewed plan is run with ``--authorize-live <plan digest>``.
"""

ROOT = __import__('pathlib').Path(__file__).resolve().parents[4]
HARNESS = __import__('pathlib').Path(__file__).resolve().parents[1]
CONDITIONS = ('baseline', 'repogi')
OUTCOMES = ('passed', 'failed', 'incomplete', 'infrastructure_failure', 'indeterminate')
