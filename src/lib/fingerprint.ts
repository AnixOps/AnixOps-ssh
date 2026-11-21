/** Comparing a host key fingerprint the user got through another channel with
 *  the one the server presented. Only SHA256 fingerprints are compared. */

const SHA256_BODY = /^[A-Za-z0-9+/]{43}=*$/;

/** The base64 body of a SHA256 fingerprint without prefix and padding, from a
 *  bare fingerprint, one with the "SHA256:" prefix, or a whole `ssh-keygen -lf`
 *  line ("256 SHA256:abc... root@host (ED25519)"). Null when none is found. */
export function normalizeFingerprint(input: string): string | null {
  for (const token of input.trim().split(/\s+/)) {
    const body = token.replace(/^SHA256:/i, "");
    if (SHA256_BODY.test(body)) return body.replace(/=+$/, "");
  }
  return null;
}

/** An MD5 fingerprint ("MD5:aa:bb:..." or 16 colon-separated hex pairs). */
export function looksLikeMd5(input: string): boolean {
  return /(^|\s)(MD5:)?([0-9a-f]{2}:){15}[0-9a-f]{2}(\s|$)/i.test(input.trim());
}

export type FingerprintMatch = "empty" | "match" | "mismatch" | "md5";

export function compareFingerprint(presented: string, pasted: string): FingerprintMatch {
  if (pasted.trim() === "") return "empty";
  if (looksLikeMd5(pasted)) return "md5";
  const a = normalizeFingerprint(presented);
  const b = normalizeFingerprint(pasted);
  return a !== null && a === b ? "match" : "mismatch";
}

/** The host key file the presented algorithm comes from, as in `ssh-keygen -lf`. */
export function hostKeyFileKind(algorithm: string): "ed25519" | "ecdsa" | "rsa" | "dsa" | null {
  const name = algorithm.toLowerCase();
  if (name.includes("ed25519")) return "ed25519";
  if (name.includes("ecdsa")) return "ecdsa";
  if (name.includes("rsa")) return "rsa";
  if (name.includes("dss") || name.includes("dsa")) return "dsa";
  return null;
}

export function keygenCommand(algorithm: string): string {
  const kind = hostKeyFileKind(algorithm);
  return kind ? `ssh-keygen -lf /etc/ssh/ssh_host_${kind}_key.pub` : "ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub";
}
