import { exitReason, type SessionEnd } from "./terminalSession";

/** First wait before retrying; each further wait doubles. */
export const BACKOFF_BASE_MS = 1000;
export const BACKOFF_CAP_MS = 30_000;
export const MAX_ATTEMPTS = 10;

/** How often the wake detector looks at the clock, and how large a gap
 *  between two looks counts as "the machine was asleep". */
export const WAKE_CHECK_MS = 5000;
export const WAKE_GAP_MS = 15_000;

/** The wait before attempt `attempt` (1-based): 1 s, 2 s, 4 s ... capped at 30 s. */
export function backoffDelayMs(attempt: number): number {
  const exponent = Math.max(0, Math.min(attempt - 1, 16));
  return Math.min(BACKOFF_BASE_MS * 2 ** exponent, BACKOFF_CAP_MS);
}

/** Only a lost connection is worth retrying: a normal exit or a signal was
 *  the user's or the server's decision. */
export function isDropped(end: SessionEnd): boolean {
  return exitReason(end) === "disconnected";
}

/** True when the clock jumped far past the check interval (sleep or suspend). */
export function wokeFromSleep(lastCheck: number, now: number): boolean {
  return now - lastCheck >= WAKE_GAP_MS;
}

export type StopReason = "cancelled" | "exhausted" | "interaction";

export type AutoReconnectState =
  | { kind: "idle" }
  /** Waiting to make attempt `attempt` at time `dueAt` (ms, same clock as the events). */
  | { kind: "waiting"; attempt: number; dueAt: number }
  /** Attempt `attempt` is opening a session. */
  | { kind: "connecting"; attempt: number }
  | { kind: "stopped"; reason: StopReason };

export type AutoReconnectEvent =
  /** The session ended. `eligible`: the setting is on, it was a lost
   *  connection of a host, and the session had been connected before (or a
   *  retry is already running). */
  | { type: "ended"; eligible: boolean; now: number }
  /** The wait is over (timer) or the person pressed "Reconnect now". */
  | { type: "fire" }
  | { type: "connected" }
  /** The network came back or the machine woke up. */
  | { type: "network"; now: number }
  /** A password, passphrase, touch or host key question appeared mid-attempt. */
  | { type: "interaction" }
  | { type: "cancel" }
  /** Manual reconnect, a new view, or the setting was turned off. */
  | { type: "reset" };

export const IDLE: AutoReconnectState = { kind: "idle" };

function nextAttempt(attempt: number, now: number): AutoReconnectState {
  return attempt >= MAX_ATTEMPTS
    ? { kind: "stopped", reason: "exhausted" }
    : { kind: "waiting", attempt: attempt + 1, dueAt: now + backoffDelayMs(attempt + 1) };
}

export function autoReconnectStep(state: AutoReconnectState, event: AutoReconnectEvent): AutoReconnectState {
  switch (event.type) {
    case "ended":
      if (state.kind === "connecting") {
        return event.eligible ? nextAttempt(state.attempt, event.now) : IDLE;
      }
      if (state.kind === "idle" && event.eligible) {
        return { kind: "waiting", attempt: 1, dueAt: event.now + backoffDelayMs(1) };
      }
      return state.kind === "waiting" ? state : IDLE;
    case "fire":
      return state.kind === "waiting" ? { kind: "connecting", attempt: state.attempt } : state;
    case "connected":
      return state.kind === "stopped" ? state : IDLE;
    case "network":
      if (state.kind === "waiting") return { ...state, dueAt: Math.min(state.dueAt, event.now) };
      // After giving up, a working network is a reason to start over.
      if (state.kind === "stopped" && state.reason === "exhausted") {
        return { kind: "waiting", attempt: 1, dueAt: event.now };
      }
      return state;
    case "interaction":
      return state.kind === "connecting" ? { kind: "stopped", reason: "interaction" } : state;
    case "cancel":
      return state.kind === "waiting" || state.kind === "connecting" ? { kind: "stopped", reason: "cancelled" } : state;
    case "reset":
      return IDLE;
  }
}

/** Whole seconds to show in the status bar; never below 1 while waiting. */
export function secondsLeft(dueAt: number, now: number): number {
  return Math.max(1, Math.ceil((dueAt - now) / 1000));
}

/** True while the status bar is showing. */
export function isActive(state: AutoReconnectState): boolean {
  return state.kind === "waiting" || state.kind === "connecting";
}
