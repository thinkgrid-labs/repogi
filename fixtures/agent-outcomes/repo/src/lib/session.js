// Sessions are kept in a caller-supplied store (a Map in tests).
export function getUserSession(store, token) {
  const session = store.get(token);
  if (!session || session.expiresAt <= Date.now()) {
    return null;
  }
  return session;
}
