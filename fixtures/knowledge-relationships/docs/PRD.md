# Authentication PRD

## REQ-AUTH-001: Session expiration
Sessions expire after inactivity. This is project intent.

### Acceptance criteria
- Expired sessions are rejected.
- Refresh respects the inactivity timeout.

### Related implementation
- [Session service](../src/session.ts)
- [Encoded path](../src/session%20policy.ts)

### Architecture
- [Policy](ADR-0004.md#ADR-0004)
- [Missing](../src/missing.ts)
- [Ambiguous](#REQ-DUP-001)
- [Unbound symbol](../src/session.ts#expireSession)
- [Outside](../../outside.ts)
- [Encoded escape](%2e%2e/%2e%2e/outside.ts)
- [Remote](https://example.invalid/session.ts)
- [Scheme](javascript:alert%281%29)
- [Malformed](../src/%ZZ.ts)

## REQ-AUTH-002: Refresh policy
Use the accepted policy.
- [Requirement](#REQ-AUTH-001)

## Ordinary heading
No requirement is invented here.

## REQ-DUP-001: First duplicate
First declaration.

## REQ-DUP-001: Second duplicate
Second declaration.

```markdown
## REQ-FAKE-001: A fenced example is not a requirement
[Fake](../src/fake.ts)
```
