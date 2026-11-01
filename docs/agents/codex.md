# Codex setup

Install and index using the [installation guide](../DEVELOPER_PREVIEW_INSTALLATION.md), then configure the client using [MCP](../MCP.md) and the [client examples](../../examples/agents/). Use absolute paths and preserve existing entries. Confirm initialization and tools-list in the client before querying.

Codex reads `~/.codex/config.toml` and, for projects you have marked trusted, the project's `.codex/config.toml`. A project file keeps the entry's fixed `--root` with its own repository. Source builds print a ready entry ([details](../DEVELOPER_EXPERIENCE.md#repogi-agent-add)):

```sh
repogi agent add codex --root "$PWD"            # prints the TOML and a per-session `codex -c ...` form
repogi agent add codex --root "$PWD" --write    # creates .codex/config.toml only if none exists
repogi doctor                                   # checks the project entry's --root
```
