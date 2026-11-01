# Set up a repository

Three commands take a repository from nothing to a connected agent. They are part of the next release, 0.1.2; build from source to use them until it is published. None of them uses the network, runs project code or package scripts, or changes memories.

```sh
cd /path/to/project
repogi init                                  # inspect, and make Git ignore the index
repogi index .                               # or: repogi init --index
repogi doctor                                # read-only health check
repogi agent add claude --root "$PWD"        # print the MCP entry for this repository
```

One `repogi` executable serves any number of repositories. Each keeps its own `.code-intel/` index and `.repogi/` memory store, and each agent entry names exactly one repository root.

## `repogi init`

```text
$ repogi init
repogi init: /home/me/ledgerlite
  repository: Git work tree
  sources:    javascript 15, jsx 1, tsx 1, typescript 1 (.ts/.tsx/.mts/.cts/.js/.jsx/.mjs/.cjs); 3 Markdown documents
  index:      not built yet
  memory:     no .repogi/memory.db yet (created by the first memory write)
  done:       wrote .code-intel/.gitignore (`*`) so Git ignores the index; no tracked file changed
next:
  repogi index /home/me/ledgerlite
  repogi doctor /home/me/ledgerlite
  repogi agent add claude --root /home/me/ledgerlite
```

- Run it anywhere inside the repository: it uses the enclosing Git work tree (or the given directory when there is none).
- It counts indexable sources and Markdown documents with the indexer's own discovery rules, reading paths only.
- When Git does not already ignore `.code-intel/`, it creates `.code-intel/.gitignore` containing `*`. Your `.gitignore` and every other tracked file stay untouched, and an existing `.code-intel/.gitignore` is kept as it is.
- It is idempotent: a second run reports `nothing to change`.
- `--dry-run` writes nothing; `--index` also builds the index (moving any index-stored memories to `.repogi/` first, exactly like `repogi index`); `--format json` returns the same report as JSON.
- It never creates `.repogi/`, never touches agent configuration and refuses symlinked state directories.

## `repogi doctor`

```text
$ repogi doctor
repogi doctor: /home/me/ledgerlite
  [ok  ] binary        repogi 0.1.1 on linux-x86_64
  [ok  ] repository    /home/me/ledgerlite (nearest index)
  [ok  ] index         schema 5, revision 1
  [ok  ] coverage      indexed javascript 15, jsx 1, tsx 1, typescript 1
  [skip] freshness     source hashes not compared (reads every source file)
                       fix: repogi doctor --freshness
  [ok  ] sources       javascript 15, jsx 1, tsx 1, typescript 1; 3 Markdown documents
  [ok  ] memory        no .repogi/memory.db (created by the first memory write)
  [ok  ] ignore        local state is ignored by Git
  [skip] agent_claude  no project .mcp.json (local and user Claude Code scopes are not inspected)
                       fix: repogi agent add claude --root /home/me/ledgerlite
  [skip] agent_codex   no project .codex/config.toml (user-wide ~/.codex is not inspected)
                       fix: repogi agent add codex --root /home/me/ledgerlite
status: healthy
```

Output from a source build; its version number stays 0.1.1 until the 0.1.2 version change.

Doctor only reads. It never indexes, migrates a store or edits configuration. Each check is `ok`, `skipped`, `unsupported`, `warning` or `error`, with a fix when it is not `ok`. The command exits non-zero only when a check is an error; `--format json` gives the same checks as JSON.

| Check | What it looks at |
| --- | --- |
| `binary` | Version, build identity and platform. Platforms without release binaries are `unsupported`. |
| `repository` | The root queries will use: the nearest ancestor with an index, else the Git work tree. |
| `state_paths` | `.code-intel` and `.repogi` must not be symlinks. |
| `index` | Present and readable, schema and extractor versions, recorded root. An index copied from another checkout is an error with the fix "move `.code-intel` aside (it is rebuildable), then re-index; keep `.repogi`". |
| `coverage` | Languages found on disk but absent from the index, such as JavaScript in an index built by 0.1.1. |
| `freshness` | Skipped unless `--freshness`, which reads and hashes every source and compares it with the index. |
| `sources` | Indexable files per language and Markdown documents (paths only). |
| `memory` | `.repogi/memory.db` readable and compatible, or memories still waiting in a schema-5 index (`repogi memory migrate`). Advice never suggests deleting the store. |
| `ignore` | Whether Git ignores `.code-intel/` and `.repogi/`. |
| `agent_claude`, `agent_codex` | A project `.mcp.json` or `.codex/config.toml` with a Repogi server: absolute, existing command and `--root` equal to this repository. User-level client settings are not read. |

## `repogi agent add`

```sh
repogi agent add claude --root /abs/path/to/project            # print the proposal
repogi agent add claude --root /abs/path/to/project --write    # create .mcp.json if it does not exist
repogi agent add codex  --root /abs/path/to/project [--write]  # .codex/config.toml
```

- The entry uses the absolute path of the running `repogi` executable and an explicit `--root`; one entry serves one repository.
- Default tools are the navigation set (`atlas_context`, `atlas_references`, `atlas_dependencies`). Pass `--tools` for others; investigation, memory and impact stay opt-in. Unknown tool names are refused.
- Without `--write`, nothing is written. With `--write`, the client file is created only when it does not exist. An existing file is never rewritten: an identical entry is reported as done, a different entry with the same name is refused, and otherwise the entry is printed for you to merge by hand.
- Both files hold absolute paths for this machine. Think twice before committing them.
- Claude Code: the output also shows the equivalent `claude mcp add --scope local …` command, which keeps the entry out of the repository. Claude Code asks you to approve project servers the first time.
- Codex: a project `.codex/config.toml` is read only after you trust the project (checked with Codex CLI 0.164). The output also shows a per-session `codex -c 'mcp_servers.repogi.command=…' -c 'mcp_servers.repogi.args=[…]'` form that writes nothing.

## Installing without Rust

Prebuilt Linux and macOS binaries need no Rust, Cargo, Node.js or Python; see [installation](DEVELOPER_PREVIEW_INSTALLATION.md). Until 0.1.2 is published, `init`, `doctor` and `agent add` require a source build.

## Configuration file

There is no `repogi.yaml`. Nothing in this release needs one: discovery follows `.gitignore` and fixed exclusions, and every command takes its options on the command line. A configuration file stays on the [roadmap](../ROADMAP.md) until a concrete need appears, and it must never widen access beyond the repository.
