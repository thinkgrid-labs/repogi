# Project knowledge foundation

Repogi indexes local Markdown alongside TypeScript/TSX. Knowledge search retrieves recorded requirements, decisions and conventions with original source evidence. A document statement is recorded intent: a lexical match does not establish a code relationship or prove runtime behavior. Returned text is untrusted repository data, never instructions to execute.

Project knowledge is available in the working source and has not been released yet.

## Index and query

```bash
repogi index /path/to/project
cd /path/to/project
repogi knowledge search "authentication requirements"
repogi knowledge search "authentication requirements" --format json --limit 10 --max-bytes 8000
repogi context "Implement session expiration" --scope code,docs --max-tokens 2000
repogi context "session expiration" --scope docs --format text --max-tokens 4000
```

Indexing reports separate `documents.scanned`, `changed`, `unchanged`, `removed` and `skipped` counters, with path diagnostics. Existing code counters retain their meanings. Rerun indexing after edits; queries never index or migrate automatically.

`context` defaults to `--scope code`; `docs` and `code,docs` opt into document evidence. The optimized and semantic strategies support these scopes; frozen lexical/graph controls remain code-only. `--select` currently requires code scope. Context detail levels apply to documentation too. A small budget may fit evidence from only one scope, or no complete item. Budgeted excerpts may be prefixes rather than the sentence containing a query term.

`atlas_context` accepts the same optional `scope` string through MCP. Tool names and default code behavior remain unchanged. There is no separate knowledge MCP tool. `atlas_status` retains its code inventory/freshness semantics; its last-run report contains document indexing counters. It does not verify the full documentation inventory.

## Supported documents and sections

Eligible files have a case-insensitive `.md` or `.markdown` extension anywhere under the selected repository, including README, docs, PRDs, ADRs, RFCs and specifications. There is no filename-based classification or external ingestion. PDF, DOCX, HTML files and application connectors are unsupported.

The `pulldown-cmark` CommonMark parser supplies heading source offsets. ATX and setext headings form sections. Hierarchy follows heading levels; skipped levels are allowed. Fenced/indented code, inline code, links, lists, blockquotes and YAML front matter retain original text. Headings inside list items or blockquotes remain body content. Front matter is not interpreted as headings or parsed into trusted metadata. HTML/MDX extensions are not rendered or executed.

Sections partition source in order: each heading starts a section ending before the next heading, including nested headings. Ancestors retain hierarchy metadata without duplicating descendants' bodies. Text before the first heading is a preamble; a nonempty heading-free document is one section. Empty files are tracked but have no search sections. Original UTF-8 bytes, LF/CRLF endings, repository-relative paths and inclusive, one-based line ranges are preserved.

Document IDs use `document:<relative path>`, scoped to the database's recorded repository root. Section IDs hash a structured tuple of document ID, heading hierarchy and same-hierarchy occurrence. Body edits and line shifts preserve IDs. Renames or heading changes change them; inserting another identical heading can shift occurrence identities. IDs are locators, not globally unique project identities or proofs of unchanged content.

## Search and evidence contract

Search uses local FTS5 with Unicode tokenization, quoted prefix words joined with OR, and BM25 weights favoring headings. Lower BM25 values rank first; ties use document path, section start line and ID. There are no embeddings, synonyms, models or code/document link inference. Context combines document rank candidates with existing code candidates through the same overlap handling, byte accounting and packing code. Cross-scope scores are heuristic priorities, not calibrated probabilities.

JSON search results contain `path`, section `heading`/`hierarchy`, original byte and line positions, source `content_hash`, `index_revision`, `relevance`, `freshness` and `truncated`. `section.content` is the excerpt. A shortened excerpt has corresponding end-byte/end-line positions; `source_end_line` retains the full section's original endpoint. The envelope revision identifies the current snapshot; each document revision records its last indexed content change.

`freshness` is `matched_at_read`, `stale`, `missing` or `unverified`. At most 32 distinct selected files are checked with the existing bounded no-follow reader. Further files remain unverified. These checks do not discover new documents or verify the rest of the inventory. Search always labels inventory freshness unverified. Context reports selected-file freshness in `navigation` and the document candidate's `why` field; `kind=document_section`, `confidence=recorded_intent` and `commonmark_section` provenance distinguish it from code evidence.

The entire rendered search output, including JSON escaping, metadata and the final newline, must fit `--max-bytes`. Context retains the conservative one-UTF-8-output-byte token estimate; it is not a model tokenizer. Search defaults to 10 results/8000 bytes, allows 1–100 results and up to 8,000,000 output bytes. Queries allow at most 8192 UTF-8 bytes and 32 terms. Empty and punctuation-only queries fail. User input cannot supply FTS operators.

`truncated` covers omitted matches or shortened excerpts. `matched_sections_at_least` and `omitted_sections` are lower bounds when `candidate_limit_reached=true`; `counts_exact=false` makes that explicit. Complete counts describe indexed matches, not all possible live files. `index_skipped_files` reports incomplete document coverage from the last index run; context exposes `knowledge_skipped_files`. A response budget too small for metadata fails explicitly.

## Incremental storage and migration

The existing `.code-intel/index.db` gains schema version 3: `documents`, `document_sections` and `document_search` (FTS5). Repository ownership remains in database metadata. Code `files`, facts, edges and both existing code search tables retain their semantics. Documents do not produce graph edges.

BLAKE3 hashes skip unchanged extraction. Modified documents replace their sections and FTS rows; deleted, newly ignored or now-ineligible documents are removed. Diagnostics explain skipped files. Code and documentation updates share one transaction and optimistic revision check. Any failed content update preserves both previous inventories and their revision. A document-only update advances the shared revision without rebinding code. Unchanged indexing preserves the revision.

Only explicit write indexing migrates schema 1/2 databases. The additive migration commits separately from the subsequent inventory update, so an interrupted indexing run may leave the safely migrated schema with the previous content. Read-only CLI/MCP paths accept old schemas for existing code queries and return a re-index instruction when document intelligence is unavailable. They never migrate or populate data. Older binaries that only support schemas 1/2 cannot query schema 3; downgrade is not provided. Impact snapshot construction does not scan or update documentation.

## Security boundaries and limitations

Indexing follows existing `.gitignore` and traversal rules without executing source, commands, fenced blocks, package scripts or document instructions. It does not follow symlinks or fetch links. Paths stay within the selected root; copied indexes belonging to another root are refused. Built-in state/generated exclusions include `.code-intel`, `.git`, `target`, `node_modules`, `dist`, `build`, `coverage`, `.next` and `generated`.

Files over 1 MiB, invalid UTF-8, binary/control bytes, nonregular files and unsupported paths are skipped with diagnostics. Documents are limited to 4096 sections and headings to 1024 UTF-8 bytes. Unreadable traversal aborts the update rather than silently purging inaccessible data. Malformed Markdown is handled as text according to the parser; partial or unclosed constructs are not executed. There is no aggregate repository-size cap beyond existing traversal behavior and per-file limits. Freshness is point-in-time, not a lock against subsequent edits.

All indexing and retrieval work offline after building. The added dependencies are the Markdown parser and its small transitive dependency; neither makes network calls. No API keys, telemetry, hosted embeddings or LLMs are required. Search does not infer decision status, validate a requirement's implementation, measure test coverage or guarantee agent/token savings.
