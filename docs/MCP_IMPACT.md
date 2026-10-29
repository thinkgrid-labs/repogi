# Opt-in MCP change impact

Enable `atlas_impact` explicitly with `--tools`. Compare Git snapshots and request bounded static impact evidence:

```text
atlas_impact {"base":"HEAD","summary":true,"tests":true,"max_bytes":8000}
```

Impact never executes project scripts or tests. Test candidates are advisory, with confirmed, possible and unknown categories; static links do not prove coverage or safety. Traversal and output caps retain omissions and uncertainty. Use `evidence_file` for selected source evidence, or a [verification manifest](VERIFICATION_MANIFEST.md) for complete retained artifact transport. See [change impact](CHANGE_IMPACT.md) and [test discovery](AFFECTED_TEST_DISCOVERY.md).
