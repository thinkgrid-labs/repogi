# Memory storage, backup and recovery

Durable memory storage is part of release 0.1.1. Until 0.1.1 is published, [build from source](DEVELOPER_PREVIEW_INSTALLATION.md#build-from-source) to use it.

Repogi keeps two kinds of local state, in two directories:

```text
repository/
├── src/ …
├── .code-intel/          rebuildable: delete it and run `repogi index` to rebuild it
│   ├── index.db          code graph, documents, knowledge, search, index revisions
│   └── impact/           Git snapshot caches for impact analysis
└── .repogi/              durable: never rebuilt, never deleted by Repogi
    ├── .gitignore        `*` (created with the directory)
    └── memory.db         reviewed project memories, their history and evidence
```

**`.repogi/memory.db` is the only authoritative copy of project memory.** Deleting or rebuilding `.code-intel/` never touches it. Neither directory is indexed as source. Both are local by default: `.repogi/` is created with a `.gitignore` that excludes everything in it, and Repogi never commits or uploads either directory. Share memory deliberately with [`repogi memory export`](#export).

## What lives where

| Data | Store | Rebuildable |
| --- | --- | --- |
| Source files, symbols, static graph, documents, knowledge entities and relationships, search indexes, index revisions | `.code-intel/index.db` | Yes, from the working tree. |
| Impact snapshots | `.code-intel/impact/` | Yes. Never contains memory records. |
| Memories, revisions, evidence snapshots, review events, supersession, corrections, memory search | `.repogi/memory.db` | **No.** Back it up. |
| Store identity, migrations, imports, rebinds; archived histories of memories imported for review | `.repogi/memory.db` | No. |

Evidence snapshots record the indexed identities and source hashes they were captured from. The store never needs the index to be readable. It needs the index only to *capture* new evidence and to *verify* existing evidence against the current code.

## The memory database

`memory.db` is an SQLite database with its own versioning:

- `PRAGMA application_id` is `0x5250474D` ("RPGM"). A file without it, such as an index or any other SQLite database put in its place, is refused with `memory_store_invalid`.
- `PRAGMA user_version` is the **memory schema** (currently 1), versioned independently of the code index schema (currently 5).
- `store_metadata` records the store kind, a random 128-bit **repository ID** (the memory lineage), the canonical repository root, and the creation time and binary. Only the root (an explicit `rebind`) and, while the store is empty, the repository ID (an explicit `--restore`) can ever change. Triggers enforce this.
- The memory tables (`memories`, `memory_revisions`, `memory_evidence`, `memory_events`, FTS5 `memory_search`) are created from the same DDL as index schema 5, so their contracts are unchanged: immutable evidence, append-only events, reviewed revisions that cannot change, and memories that are never deleted.
- `store_events` is an append-only log of creation, migrations, imports and rebinds. `memory_imports` keeps the complete source histories of memories imported for local review, verbatim and immutable.

**Journal mode.** The store uses SQLite's rollback journal (`journal_mode=DELETE`) with `synchronous=FULL`, not WAL. Between writes it is one self-contained file: no `-wal` or `-shm` files, and a copy taken while no write is running is a consistent database. A crash during a write leaves a `memory.db-journal` that SQLite rolls back on the next write open. Writers serialize through `IMMEDIATE` transactions with a 5-second busy timeout, and readers see one snapshot per command. Memory writes are small and rare, so WAL's reader/writer concurrency is not worth its extra files and backup rules here. On macOS, writes also use `fullfsync`, because a plain `fsync` there can leave committed data in the drive's cache.

**Permissions.** `.repogi/` is created `0700` and `memory.db` `0600` on Unix. Filesystems without Unix permissions (FAT, exFAT) and other platforms apply their own defaults, such as the mount's `fmask`.

## Migration from schema-5 indexes

Earlier source builds stored memories inside `.code-intel/index.db` (index schema 5). Current builds move them to `.repogi/memory.db` the first time one of these explicit writes runs:

- `repogi memory migrate`,
- any memory write (`propose`, `correct`, `approve`, `reject`, `supersede`, `retire`, `verify --record`, `import`),
- `repogi index` (before indexing, so a later rebuild or deletion cannot lose them).

Reads (`list`, `show`, `history`, `search`, `verify`, `conflicts`, `status`, `export`, `investigate --include-memory`, MCP) **never migrate and never create files**. Until migration they read the legacy rows read-only and say so (`memory_not_migrated`).

The migration algorithm:

1. Check that neither directory is a symlink and that the index belongs to this root.
2. Read every legacy row (memories, revisions, evidence, events and FTS rows) from one read snapshot of the index.
3. Create `.repogi/memory.db.migrating-<pid>-<nonce>` (owner-only), initialize the schema, insert every row verbatim (including event sequence numbers), and record a `migrated_from_index` event with the BLAKE3 digest and counts of the source rows. Then commit.
4. Reopen the temporary file and validate it: `PRAGMA integrity_check`, `PRAGMA foreign_key_check`, FTS5 `integrity-check`, every row equal to the source, the same digest and the same counts.
5. Install it with a **hard link** to `.repogi/memory.db`, which fails rather than replacing an existing file, then remove the temporary name and sync the directory. Where the filesystem has no hard links (FAT, exFAT, some network and FUSE mounts), the store is instead created at its final path and filled in **one `IMMEDIATE` transaction** that repeats the checks of step 4 before it commits. The report then carries a note saying so.
6. Freeze the legacy tables in the index with triggers that refuse inserts and updates (`project memory moved to .repogi/memory.db; this repogi binary is too old to change it`), and record `memory_migrated` in the index metadata.

The legacy rows are **never deleted**. They stay readable for comparison and recovery, and the freeze stops an older binary from writing a second, divergent copy. No index schema change removes them.

**Crash and concurrency behavior** (each case is covered by fault-injection tests):

| Interruption | Result | Recovery |
| --- | --- | --- |
| Before the temporary database commits | Nothing installed. The source is unchanged. | Run any write again. A stale temporary file older than 10 minutes is removed. |
| After commit, before validation and install | Nothing installed. The source is unchanged. | Same. |
| After install, before the source is frozen | The store is complete and validated. | The next migration finds its own `migrated_from_index` record (same digest) and only freezes the source. |
| Two processes migrate at once | One link wins. The other finds the record and reports `already_migrated`. | None needed. |
| No hard links, failure or crash inside the in-place transaction | SQLite rolls back to an empty `memory.db` (with a `memory.db-journal` after a crash). Reads still serve the legacy rows. | The next write rolls the journal back and fills the empty file in place, row for row. |
| A store already exists (for example, created after a restore) | Rows are reconciled in one `IMMEDIATE` transaction: missing memories are added, older copies are fast-forwarded, and divergent histories refuse the whole migration (`migration_conflict`) with both copies unchanged. | Export the store, then resolve explicitly. |

A partially written destination can never be mistaken for a complete one: the store path appears only after validation (or holds an empty, uninitialized database until the in-place transaction commits), and an existing store is changed only by one committed transaction.

## Rebuilding or deleting the index

```sh
repogi index .
repogi memory list
rm -rf .code-intel          # the rebuildable index only
repogi memory list          # same memories and history; evidence reported `unverified`
repogi index .
repogi memory verify --all  # identities re-resolved in the new index
```

Without an index, memory reads still work. Every evidence item is `unverified` with the change `index_unavailable`, responses carry a `storage` note, and text headers say `no index`. Capturing evidence (`propose`, `correct`) and approval need the index and fail with `index_required`. Reject, supersede and retire do not.

After a rebuild, evidence is re-resolved by its recorded exact identities and cited byte hashes, never by name similarity. Index revisions restart when an index is recreated. A revision lower than the one at capture is reported as `index_rebuilt`, an informational change that does not make unchanged evidence stale. A rebuild never establishes that a claim is still correct: changed bytes are `stale` and removed identities are `missing`, exactly as before. Memories are never re-approved, rewritten or invalidated by indexing.

| You see | Meaning |
| --- | --- |
| `current` | The identity resolves and the cited bytes are unchanged. |
| `current` + `identity_relocated` / `index_rebuilt` | Re-resolved in a rebuilt or changed index. The bytes still match. |
| `stale` | The evidence exists but changed (bytes, decision status, relationship target). |
| `missing` | The identity or file is gone. |
| `unresolved` | The recorded identity is now ambiguous. |
| `unverified` (+ `index_unavailable`) | It cannot be checked: no index, unreadable file, or read cap. |
| warning `stale_evidence` on an approved memory | Requires review: correct, supersede or retire it. |

## Export

```sh
repogi memory export --output repogi-memory.json [--force]
```

An export is a complete backup of one store: every memory with all revisions (approved, pending, rejected), evidence snapshots, review events, supersession links and corrections. It is read-only for the store and works on an unmigrated (legacy) store too.

Output safety:

- An existing file is refused (`output_exists`) unless `--force` is given. A symlink at the output path is always refused (`output_rejected`). Parent directories are resolved normally.
- The data is written to an owner-only (`0600`) temporary file in the same directory and synced. It is then linked into place (no clobber) or, with `--force`, renamed over the old file. A failure leaves no partial export.
- Where hard links are unavailable (FAT or exFAT removable drives, some network shares), the export is created exclusively at the final name (`O_EXCL`) and written there, so an existing file is still never replaced. A write error removes it; a crash or power loss can leave a partial file, which import refuses (truncated JSON or digest mismatch).
- Exports contain no absolute paths. Evidence paths are repository-relative.

**Exports can contain sensitive project knowledge and reviewer labels.** Store and share them like source code.

### Format (version 1)

```json
{
  "format": "repogi-memory-export",
  "format_version": 1,
  "notice": "…",
  "created_at": "2026-10-31T12:00:00Z",
  "created_by": "repogi 0.1.0",
  "source": {"repository_id": "<32 hex>|null", "store": "durable|legacy_index", "memory_schema": 1},
  "counts": {"memories": 7, "revisions": 8, "evidence": 9, "events": 17},
  "memories": [
    {"id": "mem-0001", "memory_type": "…", "status": "…", "latest_revision": 1,
     "approved_revision": 1, "pending_revision": null, "superseded_by": null,
     "created_at": "…", "updated_at": "…", "version": 2,
     "revisions": [{"revision": 1, "state": "approved", "digest": "…", "memory_type": "…",
                    "title": "…", "claim": "…", "assertion": null, "index_revision": 1,
                    "created_at": "…", "proposed_by": "…", "reason": null, "evidence": [ … ]}],
     "events": [{"revision": 1, "action": "approved", "actor": "alice", "at": "…",
                 "digest": "…", "note": null, "detail": {"verification": "current"}}]}
  ],
  "integrity": {"algorithm": "blake3", "encoding": "repogi-canonical-json-v1", "digest": "…"}
}
```

Memories are ordered by ID, revisions by number and events oldest first. Event sequence numbers are store-local and not exported; their order is.

**`integrity.digest`** is BLAKE3 over the compact JSON encoding (`repogi-canonical-json-v1`: no whitespace, object keys in the struct field order shown above, `detail` maps with sorted keys, `serde_json` string escaping) of the array `[format_version, source, memories]`. Each revision's own `digest` is the claim digest that approval names. Import recomputes both.

What the digests establish: the file is the export Repogi produced, without accidental or casual modification, and each claim matches its evidence snapshots. What they do **not** establish: who made the export, whether reviewers were who they claimed to be, or that nobody deliberately replaced the file and recomputed the digests. The digest is unkeyed. It is not a signature.

Before writing, an export checks that it is lossless: every memory is converted back to stored rows and compared with the rows it came from (`export_inconsistent` otherwise).

## Import and restore

```sh
repogi memory import repogi-memory.json [--restore] [--dry-run] [--actor <label>]
```

Import is an explicit write. Validation runs before anything is written:

- The format, format version (`import_unsupported` if unknown) and memory schema are checked.
- The size limit is 256 MiB, checked before reading. Other limits: 100,000 memories, 10,000 revisions and 100,000 events per memory, and JSON nesting depth.
- The input must be valid UTF-8, with no unknown fields anywhere.
- The integrity digest must match (`integrity_mismatch`) and record counts must agree.
- Memory IDs must be valid and unique. Revisions must be numbered 1…n with consistent types, and every claim digest must match its claim and evidence (`digest_mismatch`).
- Lifecycle states must be consistent: a pending revision is the only `proposed` one, the approved revision is `approved`, and statuses match their revisions and supersession.
- Titles, claims, notes, actors, timestamps and every evidence string are bounded and free of control characters. Evidence paths must be repository-relative, with no `..`, root or backslash.
- Supersession targets must be imported or already present.

Imported text is data. Nothing in it is executed, followed or fetched, and evidence is never re-bound to similarly named files.

One import is one `IMMEDIATE` transaction: it is applied completely or not at all. An interrupted import is rolled back by SQLite. `--dry-run` runs the same validation and reconciliation, then rolls back. On a repository whose memories are still in a legacy index, it refuses (`migration_required`) rather than migrating.

### Restore (same memory lineage)

Restore keeps memory IDs, review states and complete histories. It applies when:

- the export's repository ID equals this store's (automatic), or
- `--restore` asserts that the export is this repository's own memory. That is allowed into an absent or empty store, which then adopts the export's repository ID, or into a store of the same lineage. Into a store of another lineage that already holds memories it is refused with `repository_mismatch`.

Each memory is reconciled:

| Destination | Outcome |
| --- | --- |
| Absent | `created` |
| Identical (ignoring store-local sequences) | `unchanged`, so repeated imports are idempotent |
| A strict prefix of the export (more revisions or events; a pending revision since reviewed) | `fast_forwarded` |
| Newer than the export | `store_newer`: nothing changes |
| Both changed | conflict: the **whole import is refused** (`import_conflict`) and nothing is written |

Approved memory and history are never overwritten.

### Review import (another or unknown lineage)

Without `--restore`, a backup from another lineage, or one whose lineage cannot be established, never becomes authoritative:

- Each source memory that is approved (its approved claim) or pending (its pending claim) becomes a **new local memory** with a fresh ID, in `pending_review`. Its revision 1 has the same claim and evidence snapshots, and therefore the same digest. Its history starts with an `imported` event naming the source repository, memory, revision, status and export digest.
- The complete source history, including source approvals and reviewers, is archived and shown by `memory history` under `imported_from` as **history only**. Rejected, superseded and retired source memories are archived without a local memory.
- A claim already recorded locally is skipped (`duplicate_claim`). Re-importing the same export changes nothing (`already_imported`).
- A local reviewer must approve each one. Approval re-verifies the evidence against this repository, so evidence that does not exist here cannot be approved and the claim must be corrected.

The report lists every memory with its outcome and its evidence state in this repository now (`current`, `stale`, `missing`, `unresolved` or `unverified`).

### Moving, cloning and copying repositories

| Situation | What to do |
| --- | --- |
| Restore after deleting `.repogi` | `repogi memory import <backup> --restore`. |
| The repository moved to another path | Memory commands refuse with `memory_root_mismatch` and print the recorded root. Run `repogi memory rebind --from <recorded root>`, then `repogi index .`. Rebind refuses if the old path still holds the same store (`memory_store_copied`), because that is a copy, not a move. |
| A clone or copy of the repository | In the original, `repogi memory export`. In the copy, move any copied `.repogi` aside, then `repogi memory import <backup> --restore`. A copied `memory.db` is refused at the new path. |
| A different repository | `repogi memory import <backup>` (review import). |

Path similarity is never treated as proof that two repositories are the same.

## Approval consistency

Approval runs inside one memory write transaction:

1. Begin `IMMEDIATE` on `memory.db`, which excludes every other memory writer.
2. Re-read the memory, its pending revision and the stored digest. Check the requested digest prefix (at least 12 hex characters).
3. Recompute the claim digest from the stored claim and evidence. A mismatch means the store is damaged (`digest_invalid`).
4. Require backing evidence. Re-verify every backing item against the code index and the working tree. The index was opened read-only earlier, so all of its reads in this command come from one SQLite read snapshot.
5. Refuse (`evidence_not_current`, `index_required`) or write the state change and the `approved` event (with the verification result and index revision) together, then commit.

Concurrent reviews of one revision commit exactly once. The loser sees `nothing_to_review` or `digest_mismatch`, and a correction attempted while a revision is pending sees `review_pending`. Refused transitions change nothing.

The boundary: the memory transaction and the index read snapshot are **separate SQLite databases**. Repogi does not attach them or claim a cross-database atomic commit. While approval holds its index read snapshot, an index writer waits (rollback journal) for at most its busy timeout. The working tree cannot be frozen: an edit between the byte check and the commit (milliseconds) is not detected at approval, but the digest still binds the exact snapshot, and the next read reports the change.

## Reviewer identity

`--reviewer` / `REPOGI_MEMORY_REVIEWER` is a **self-reported label**, recorded in the history. It is not authentication. Approval is deliberate (an exact digest, an explicit command, a named label, an atomic event), but anyone, or any agent, with shell access to the repository can run the CLI. MCP never exposes approval, rejection, correction, import, migration or any other write.

### Design note: signed reviews (future, not implemented)

Signed reviews would make a review event verifiable by others. They need:

- **Keys.** A per-reviewer signing key (for example Ed25519, or SSH/Git signing keys) held outside the repository, ideally in an agent or hardware token so that a coding agent with shell access cannot use it unattended.
- **Signed payload.** Canonical bytes of (repository ID, memory ID, revision, claim digest, action, timestamp, reviewer key ID), stored with the event. Signatures must cover the claim digest, not the label.
- **Trust model.** A versioned, reviewed allow-list of reviewer public keys (for example in the repository, changed only through code review), with rotation and revocation dates, so that verification can answer "was this key trusted for this repository when it signed".
- **Verification.** Reads would report `signature: valid|invalid|untrusted|absent` per review, and exports would carry signatures unchanged. An unsigned export digest stays an integrity check only.

Until then, treat approvals as auditable intent, not authorization.

## Impact snapshots

Impact analysis seeds snapshot databases with consistent copies of an index. Copies made by those earlier builds included the memory tables. Now every copy is scrubbed: memory tables are dropped with `secure_delete` (freed pages zeroed) and recreated empty, and a copy that cannot be scrubbed is deleted. Opening the cache also scrubs snapshots left by older builds. Snapshots hold only code-analysis data, and a test checks the bytes of every cache file for a memory claim.

## Corruption and recovery

- `repogi memory status --check` runs `PRAGMA integrity_check`, `PRAGMA foreign_key_check` and a row-count consistency check of the search index, read-only, and lists problems. FTS5's own `integrity-check` needs a writable connection, so it runs only when a migration validates a new store, not in `status --check`.
- A damaged, foreign or truncated `memory.db` is refused by reads and writes and **never overwritten**. To recover: move it aside (do not delete it), then `repogi memory import <latest backup> --restore`. If there is no backup, keep the damaged file. SQLite's `.recover` can often salvage rows.
- If `memory.db-journal` exists after a crash, keep it next to the database. The next write rolls back the interrupted transaction. Do not delete the journal by hand.
- Legacy rows in an earlier index stay available as a last-resort source: restore an old index into `.code-intel/` and run `repogi memory migrate` to merge them.

### Back up before deleting `.code-intel`

```sh
repogi memory status          # where are memories stored?
repogi memory migrate         # if status says they are still in the index
repogi memory export --output ~/backups/$(basename "$PWD")-memory-$(date +%F).json
rm -rf .code-intel            # safe: memories are in .repogi/memory.db
```

For a file-level backup, copy `.repogi/memory.db` while no memory write is running (rollback-journal mode makes the quiescent file consistent), or use `sqlite3 .repogi/memory.db ".backup <file>"`.

## Compatibility and downgrade

- Release 0.1.0 (index schema 2) refuses schema-5 indexes for every command, including MCP, changes nothing and never reads `.repogi/`. Delete `.code-intel/` and re-index to use 0.1.0 again; `.repogi/memory.db` stays untouched.
- The index schema stays 5, so earlier source builds with index-stored memory can still open current indexes. They see only the frozen legacy rows (memory as it was when migrated, without later changes), and their memory writes fail with `project memory moved to .repogi/memory.db; this repogi binary is too old to change it`. They do not read `.repogi/`.
- A memory store with a newer schema is refused by both reads and writes (`memory_store_newer`) and is never downgraded. Use a compatible binary, or export with the newer binary and import with one whose export format matches.
- There is no automatic downgrade path, and none is planned.

## Platform and security limitations

- Migration and exports prefer hard links and fall back to the transactional or exclusive-create paths above where linking is unavailable. Both paths were exercised on a real Linux `vfat` mount. They have not been run on macOS or Windows filesystems, network shares or FUSE mounts.
- Symlink refusal covers the `.code-intel` and `.repogi` directories, both databases and their SQLite side files. Checks and opens use no-follow flags, but a directory replaced between the check and the open by a process with write access to the repository is outside this model, as for the index.
- Permission restriction (`0700`/`0600`) applies on Unix filesystems with Unix permissions only. On FAT/exFAT the mount options decide who can read `memory.db`.
- The repository ID identifies a memory lineage, not a repository. Two clones restored from one backup share it, so a later backup can move between them as a restore.
- Export, restore and migration hold the complete memory set in memory at once. This was measured at 1,000 memories (about 2.5 MB). Much larger stores have not been tested, and streaming is not implemented.
- Fault injection (`REPOGI_TEST_FAULT`) exists only in debug builds.
