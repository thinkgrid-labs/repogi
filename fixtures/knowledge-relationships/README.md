# Knowledge relationship evaluation fixture

This local-only labeled corpus contains explicit requirements, duplicate identifiers, ADR statuses, supersession cycles, TypeScript declarations and a static test consumer. `cases.json` labels every non-containment reference and every requirement occurrence. Ordinary headings and fenced examples are negative extraction examples.

Run the evaluator from the Repogi repository root:

```sh
python3 scripts/evaluation/knowledge_relationships.py target/release/repogi --output /tmp/repogi-relationship-results
```

The evaluator copies this fixture to a temporary directory. It never runs its TypeScript or tests. It fails on any false validated target. A validated link proves target identity only; no requirement satisfaction or test coverage is established.
