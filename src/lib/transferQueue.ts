/**
 * The file pane's transfer queue: how many transfers of one host run at once,
 * which queued ones start next, and the numbers shown for a running one
 * (speed, time left). Pure functions, so they are tested without a window.
 */

/**
 * Transfers of one host running at once. Every transfer opens an SFTP channel
 * on the host's shared connection, and OpenSSH refuses more than `MaxSessions`
 * (default 10) channels on one connection, with the terminal and tunnels
 * sharing them. Four leaves room for those.
 */
export const DEFAULT_TRANSFER_LIMIT = 4;

export type QueueState = "queued" | "running" | "done" | "failed" | "cancelled";

/** The ids of queued items to start now, oldest first, up to `limit` running. */
export function pickToStart(items: { id: string; state: QueueState }[], limit: number): string[] {
  const running = items.filter((item) => item.state === "running").length;
  const free = Math.max(0, Math.floor(limit) - running);
  return items
    .filter((item) => item.state === "queued")
    .slice(0, free)
    .map((item) => item.id);
}

export interface Rate {
  /** Time of the last sample (ms). */
  at: number;
  bytes: number;
  /** Smoothed bytes per second; 0 until there are two samples. */
  perSecond: number;
}

/** Folds a new progress sample into the smoothed speed. */
export function nextRate(previous: Rate | undefined, bytes: number, now: number): Rate {
  if (!previous) return { at: now, bytes, perSecond: 0 };
  const seconds = (now - previous.at) / 1000;
  if (seconds <= 0) return { ...previous, bytes };
  const instant = Math.max(0, bytes - previous.bytes) / seconds;
  const perSecond = previous.perSecond === 0 ? instant : previous.perSecond * 0.7 + instant * 0.3;
  return { at: now, bytes, perSecond };
}

/** Seconds left, or null when the total or the speed is not known. */
export function secondsLeft(bytes: number, total: number | null, perSecond: number): number | null {
  if (total === null || total <= 0 || perSecond <= 0) return null;
  return Math.max(0, Math.ceil((total - bytes) / perSecond));
}

/** `45 s`, `3 min 5 s`, `1 h 2 min`. */
export function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ${seconds % 60} s`;
  return `${Math.floor(minutes / 60)} h ${minutes % 60} min`;
}

/**
 * A failure message that names the remote path and the reason. Messages that
 * already mention the path (the daemon adds it) are kept as they are.
 */
export function failureMessage(remote: string, reason: unknown): string {
  const text = (reason instanceof Error ? reason.message : String(reason)).trim() || "unknown error";
  return text.includes(remote) ? text : `${remote}: ${text}`;
}
