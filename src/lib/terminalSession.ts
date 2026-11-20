import { t } from "./i18n";

/** Written before a reconnect, so whatever the old session left on is undone:
 *  leave the alternate screen, soft reset (DECSTR: cursor, scroll region,
 *  attributes, insert and origin modes), and turn off mouse reporting
 *  (1000/1002/1003/1006), bracketed paste (2004) and focus events (1004).
 *  It starts with DECSC: leaving the alternate screen restores the saved
 *  cursor, which outside of it would be the top-left corner, and the new
 *  session would then print over the old screen. */
export const TERMINAL_RESET =
  "\x1b7\x1b[?1049l\x1b[!p\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1004l";

/** How long a view waits for resizing to settle before telling the session. */
export const RESIZE_DEBOUNCE_MS = 50;

export interface Debounced<A extends unknown[]> {
  (...args: A): void;
  /** Drops a pending call. */
  cancel: () => void;
}

/** Calls `fn` once, with the last arguments, after `ms` without another call. */
export function trailingDebounce<A extends unknown[]>(fn: (...args: A) => void, ms: number): Debounced<A> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const debounced = (...args: A) => {
    if (timer !== undefined) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = undefined;
      fn(...args);
    }, ms);
  };
  debounced.cancel = () => {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
  };
  return debounced;
}

export type ExitReason = "exit" | "signal" | "disconnected";

/** How a session ended, as the backend reports it. `reason` is missing from
 *  older backends: then a code means an exit and none a lost connection. */
export interface SessionEnd {
  code: number | null;
  reason?: ExitReason;
  signal?: string;
}

export function exitReason(end: SessionEnd): ExitReason {
  return end.reason ?? (end.code === null ? "disconnected" : "exit");
}

/** The status card's title: "Connection lost", "Session ended (exit 1)" or "Killed by signal SIGTERM". */
export function exitTitle(end: SessionEnd): string {
  switch (exitReason(end)) {
    case "signal": {
      const name = end.signal ? `SIG${end.signal.replace(/^SIG/, "")}` : "?";
      return t.sessionKilled.replace("{signal}", name);
    }
    case "disconnected":
      return t.connectionLost;
    default:
      return end.code === null ? t.sessionEnded : t.sessionEndedCode.replace("{code}", String(end.code));
  }
}
