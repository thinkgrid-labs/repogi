export function credentialDigest(credential: string): string {
  return `digest:${credential.length}`;
}
