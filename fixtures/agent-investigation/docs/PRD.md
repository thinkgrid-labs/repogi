# Authentication PRD

## REQ-AUTH-001: Session expiration
Sessions expire after a period of inactivity. Expired sessions must be rejected by every consumer.

### Acceptance criteria
- A session idle longer than the timeout is expired.
- Expired sessions are rejected by the request middleware.

### Related implementation
- [Session expiration](../src/auth/session.ts)

## REQ-AUTH-002: Token refresh
Access tokens are refreshed only for sessions that have not expired.

### Acceptance criteria
- Refresh failures are reported for an expired session.

### Related implementation
- [Token refresh](../src/auth/token.ts)
- [Governing requirement](#REQ-AUTH-001)

## REQ-AUTH-003: Login validation
Login validates the user name and password before a session is created.

### Related implementation
- [Login validation](../src/auth/login.ts)

## REQ-AUDIT-001: Audit trail
Authentication events are recorded in an audit trail.

### Related implementation
- [Audit writer](../src/audit/writer.ts)
- [Audit policy](#REQ-DUP-001)
- [Audit sink](../src/auth/service.ts#auditSink)

## REQ-DUP-001: Retention
Audit records are retained for one year.

## REQ-DUP-001: Retention copy
A conflicting duplicate declaration.
