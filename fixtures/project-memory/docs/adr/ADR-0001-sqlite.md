---
id: ADR-0001
status: accepted
implementation: ../../src/storage/sqliteStore.ts
---
# Store sessions in SQLite
## Context
Sessions must survive process restarts without an external server.
## Decision
Use an embedded SQLite database file for durable storage.
## Consequences
Deployments ship one database file per instance.
