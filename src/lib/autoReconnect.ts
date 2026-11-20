import type { SessionEnd } from "./terminalSession";
import { exitReason } from "./terminalSession";

/** The first wait, and the longest one: 1 s, 2 s, 4 s ... then every 30 s. */
export const RECONNECT_BASE_MS = 1_000;
export const RECONNECT_MAX_DELAY_MS = 30_000;
/** Auto-reconnect gives up after this long without a working session, or this many tries. */
export const RECONNECT_GIVE_UP_MS = 5 * 60_000;
export const RECONNECT_MAX_ATTEMPTS = 20;

/** The wait before attempt `attempt` (1 is the first), doubling up to the cap. */
export function backoffDelay(attempt: number): number {
  const step = Math.max(0, Math.floor(attempt) - 1);
  return Math.min(RECONNECT_BASE_MS * 2 ** Math.min(step, 16), RECONNECT_MAX_DELAY_MS);
}

/** A scheduled attempt: which one, when it starts, and when the chain began. */
export interface RetryPlan {
  attempt: number;
  dueAt: number;
  startedAt: number;
}

/** The chain starts when a working session is lost: attempt 1 after the base delay. */
export function beginRetry(now: number): RetryPlan {
  return { attempt: 1, dueAt: now + backoffDelay(1), startedAt: now };
}

/** An attempt failed with a network error: the next one, or null to give up. */
export function nextRetry(plan: RetryPlan, now: number): RetryPlan | null {
  const attempt = plan.attempt + 1;
  if (attempt > RECONNECT_MAX_ATTEMPTS || now - plan.startedAt >= RECONNECT_GIVE_UP_MS) return null;
  return { attempt, dueAt: now + backoffDelay(attempt), startedAt: plan.startedAt };
}

/** The network is back (online event, wake from sleep): try at once, and start
 *  the backoff afresh since the earlier failures say nothing about the new link. */
export function nudgeRetry(now: number): RetryPlan {
  return { attempt: 1, dueAt: now, startedAt: now };
}

/** The tab came back into view: do the waiting attempt now, counting it as the same chain. */
export function retryNow(plan: RetryPlan, now: number): RetryPlan {
  return { ...plan, dueAt: Math.min(plan.dueAt, now) };
}

/** Whole seconds until `dueAt`, never negative; rounded up so it ends on 1, not 0. */
export function secondsLeft(plan: RetryPlan, now: number): number {
  return Math.max(0, Math.ceil((plan.dueAt - now) / 1000));
}

export type FailureKind = "network" | "auth" | "hostkey" | "vault" | "cancelled" | "other";

const PATTERNS: [FailureKind, RegExp][] = [
  ["vault", /vault is locked/i],
  ["cancelled", /cancel/i],
  ["hostkey", /host key|hostkey|known_hosts|fingerprint/i],
  ["auth", /auth|permission denied|password|passphrase|publickey|no supported methods|access denied/i],
  [
    "network",
    /timed? ?out|connection (refused|reset|closed|aborted|lost)|unreachable|no route|network|broken pipe|resolve|name or service|lookup|not known|disconnected|eof|keepalive/i,
  ],
];

/** Sorts the message of a failed connect. Only "network" is worth retrying:
 *  the others need the user (credentials, a host key decision, the vault). */
export function classifyFailure(message: string): FailureKind {
  for (const [kind, pattern] of PATTERNS) if (pattern.test(message)) return kind;
  return "other";
}

/** What the view knows when a session ends. */
export interface EndContext {
  /** The setting is on. */
  enabled: boolean;
  /** A saved host, not a local shell. */
  remote: boolean;
  /** The session (or an earlier try of this chain) was open before it ended. */
  wasConnected: boolean;
  /** A password, passphrase, key PIN or host key question came up for this connection. */
  prompted: boolean;
}

export type RetryDecision = { retry: true } | { retry: false; why: "disabled" | "local" | "clean" | "signal" | "never-connected" | "prompted" | FailureKind };

/** Decides whether an ended session is retried by itself. `error` is set when
 *  the connect itself failed; otherwise the end event says how the session ended. */
export function decideRetry(end: SessionEnd & { error?: string }, ctx: EndContext): RetryDecision {
  if (!ctx.enabled) return { retry: false, why: "disabled" };
  if (!ctx.remote) return { retry: false, why: "local" };
  if (ctx.prompted) return { retry: false, why: "prompted" };
  if (end.error !== undefined) {
    const kind = classifyFailure(end.error);
    return kind === "network" && ctx.wasConnected ? { retry: true } : { retry: false, why: kind === "network" ? "never-connected" : kind };
  }
  switch (exitReason(end)) {
    case "exit":
      return { retry: false, why: "clean" };
    case "signal":
      return { retry: false, why: "signal" };
    default:
      return ctx.wasConnected ? { retry: true } : { retry: false, why: "never-connected" };
  }
}
