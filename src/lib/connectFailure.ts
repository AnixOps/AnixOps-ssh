/** Sorts the message of a failed connect into the stage that failed, for both
 *  engines: the built-in one (crates/easyssh-ssh/src/error.rs) and the OpenSSH
 *  one (the stderr of ssh). */
export type FailureStage =
  | "dns"
  | "connect"
  | "hostkey"
  | "auth"
  | "session"
  | "vault"
  | "cancelled"
  | "other";

const STAGE_PATTERNS: [FailureStage, RegExp][] = [
  ["vault", /vault is locked/i],
  ["cancelled", /cancel/i],
  ["hostkey", /host key|hostkey|known_hosts|fingerprint/i],
  ["auth", /auth|permission denied|password|passphrase|publickey|no supported methods|access denied/i],
  ["dns", /resolve|name or service|lookup|not known|nodename|no address associated/i],
  [
    "connect",
    /timed? ?out|connection (refused|reset|closed|aborted|lost)|unreachable|no route|network|broken pipe|disconnected|eof|keepalive|proxy/i,
  ],
  ["session", /channel|protocol error|sftp|open failed|administratively prohibited/i],
];

export function classifyStage(message: string): FailureStage {
  for (const [stage, pattern] of STAGE_PATTERNS) if (pattern.test(message)) return stage;
  return "other";
}

/** The methods the engine says it tried: "authentication failed (tried: agent,
 *  identity file ~/.ssh/id_ed25519); note" gives ["agent", "identity file ~/.ssh/id_ed25519"].
 *  Empty when the message has no such list or says "nothing". */
export function triedMethods(message: string): string[] {
  const match = /\(tried: (.*?)\)(?:;|$)/.exec(message) ?? /\(tried: (.*)\)/.exec(message);
  if (!match || match[1].trim() === "nothing") return [];
  return match[1]
    .split(", ")
    .map((method) => method.trim())
    .filter(Boolean);
}
