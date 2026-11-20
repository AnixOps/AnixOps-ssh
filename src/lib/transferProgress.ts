/**
 * The numbers shown for a running transfer (speed, time left) and the wording
 * of its failure. Pure functions, so they are tested without a window. The
 * queue itself lives in the backend, per host.
 */

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
