# Claude Code setup

Install and index using the [installation guide](../DEVELOPER_PREVIEW_INSTALLATION.md), then configure the client using [MCP](../MCP.md) and the [client examples](../../examples/agents/). Use absolute paths and preserve existing entries. Confirm initialization and tools-list in the client before querying.

Source builds print a ready entry for one repository, with the running executable's absolute path ([details](../DEVELOPER_EXPERIENCE.md#repogi-agent-add)):

```sh
repogi agent add claude --root "$PWD"            # prints a project .mcp.json and the `claude mcp add --scope local` form
repogi agent add claude --root "$PWD" --write    # creates .mcp.json only if none exists
repogi doctor                                    # checks the project entry's command and --root
```
