# Authentication architecture

## Session revocation
The session registry removes revoked credentials before authenticating a request.
The HTTP layer returns an unauthorized response for expired credentials.
