---
id: ADR-0003
status: accepted
supersedes: ADR-0002
relates_to: REQ-AUTH-001
implementation: ../../src/auth/session.ts
---
# Inactivity session timeout
## Context
Absolute timeouts logged out active users.
## Decision
Sessions expire after a fixed period of inactivity.
## Consequences
Every session consumer must check expiration before use.
