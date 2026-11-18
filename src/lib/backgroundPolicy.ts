/* What a terminal does when the app leaves and returns to the screen.

   A phone does not keep a TCP connection for an app that is not in front (see
   docs/mobile-prototype.md, "Background execution"), so the app does not pretend
   to. It keeps the connection for a grace period the person chooses (a quick
   look at a message should not cost a session), then closes it cleanly so no
   half-open socket is left behind. On return it replaces the connection
   quickly, and says what happened.

   This file is the policy, with the clock and the timers passed in so tests
   can run it without waiting. */

/** Choices for "keep the connection in the background", in seconds. -1 is
 *  "as long as the system lets it live". */
export const GRACE_CHOICES = [0, 30, 60, 300, 900, -1] as const;
export const DEFAULT_GRACE_SECONDS = 60;

/** Away for at least this long (ms), a connection that looks alive may be
 *  dead (NAT timeouts, the radio was off), so an attached session replaces it. */
export const PROBE_AFTER_MS = 10_000;

export const KEEPALIVE_CHOICES = [0, 15, 30, 60] as const;
export const DEFAULT_KEEPALIVE_SECONDS = 30;

export interface Timers {
  now(): number;
  setTimeout(callback: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

export const realTimers: Timers = {
  now: () => Date.now(),
  setTimeout: (callback, ms) => window.setTimeout(callback, ms),
  clearTimeout: (handle) => window.clearTimeout(handle as number),
};

export interface ResumeInfo {
  /** How long the app was away, ms. */
  awayMs: number;
  /** The grace period ran out and the connection was closed. */
  expired: boolean;
}

export type ResumeAction =
  /** Nothing to do: the connection is still there. */
  | "none"
  /** Replace the connection now (attached sessions only). */
  | "reconnect"
  /** Tell the person the session ended and offer a new one. */
  | "offer";

/** What to do when the app is back. `alive` is whether the app still holds a
 *  session that has not reported an end. */
export function resumeAction(info: ResumeInfo, attached: boolean, alive: boolean): ResumeAction {
  if (info.expired || !alive) return attached ? "reconnect" : "offer";
  if (attached && info.awayMs >= PROBE_AFTER_MS) return "reconnect";
  return "none";
}

/** Tracks hidden and visible and fires the grace timer. */
export class BackgroundPolicy {
  private hiddenAt: number | null = null;
  private timer: unknown = null;
  private expired = false;

  constructor(
    private graceSeconds: number,
    private readonly callbacks: {
      /** The grace period ended while hidden: close the connection. */
      onExpire: () => void;
      /** The app is visible again. */
      onResume: (info: ResumeInfo) => void;
    },
    private readonly timers: Timers = realTimers,
  ) {}

  setGrace(seconds: number) {
    this.graceSeconds = seconds;
    // A change while hidden takes effect for the time still to run.
    if (this.hiddenAt !== null && !this.expired) this.arm();
  }

  get isHidden(): boolean {
    return this.hiddenAt !== null;
  }

  hidden() {
    if (this.hiddenAt !== null) return;
    this.hiddenAt = this.timers.now();
    this.expired = false;
    this.arm();
  }

  visible() {
    if (this.hiddenAt === null) return;
    this.disarm();
    const info: ResumeInfo = { awayMs: Math.max(0, this.timers.now() - this.hiddenAt), expired: this.expired };
    this.hiddenAt = null;
    this.expired = false;
    this.callbacks.onResume(info);
  }

  dispose() {
    this.disarm();
    this.hiddenAt = null;
  }

  private arm() {
    this.disarm();
    if (this.graceSeconds < 0 || this.hiddenAt === null) return;
    const elapsed = this.timers.now() - this.hiddenAt;
    const remaining = Math.max(0, this.graceSeconds * 1000 - elapsed);
    this.timer = this.timers.setTimeout(() => {
      this.timer = null;
      if (this.hiddenAt === null || this.expired) return;
      this.expired = true;
      this.callbacks.onExpire();
    }, remaining);
  }

  private disarm() {
    if (this.timer !== null) this.timers.clearTimeout(this.timer);
    this.timer = null;
  }
}
