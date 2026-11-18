/* Telling touch gestures apart on the terminal, with no DOM and no timers of
   its own (the caller passes the time and calls `tick`), so a sequence of
   touches can be replayed in a test.

   One finger
     tap          short touch, no movement: focus the terminal, show the keyboard
     scroll       drag past the slop: scroll the scrollback, or move through a
                  full-screen program (less, vim, tmux) with its own keys
     long press   hold still: select the word under the finger; the same
                  touch can then drag to extend the selection
   Two fingers
     scroll       both move together: scroll faster (pages)
     pinch        the distance changes: change the font size */

export interface Pt {
  x: number;
  y: number;
}

export type GestureEvent =
  | { type: "tap"; at: Pt }
  | { type: "longpress"; at: Pt }
  | { type: "select-move"; at: Pt }
  | { type: "select-end"; at: Pt }
  /** `dy` is how far the fingers moved since the last event, in pixels
   *  (positive: down); the content follows the fingers. */
  | { type: "scroll"; fingers: 1 | 2; dy: number }
  /** `scale` is the distance between the fingers now over the distance at the start. */
  | { type: "pinch"; scale: number }
  | { type: "pinch-end"; scale: number };

type Phase = "idle" | "pending" | "scroll" | "select" | "two" | "pinch" | "scroll2" | "done";

export const SLOP_PX = 10;
export const LONG_PRESS_MS = 450;

const distance = (a: Pt, b: Pt) => Math.hypot(a.x - b.x, a.y - b.y);
const middle = (a: Pt, b: Pt): Pt => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });

export class GestureTracker {
  private phase: Phase = "idle";
  private startedAt = 0;
  private origin: Pt = { x: 0, y: 0 };
  private last: Pt = { x: 0, y: 0 };
  private startDistance = 1;
  private lastScale = 1;
  private startMid: Pt = { x: 0, y: 0 };

  constructor(
    private readonly slop = SLOP_PX,
    private readonly longPressMs = LONG_PRESS_MS,
  ) {}

  /** When the long press fires if nothing moves, or `null` when no touch is waiting for it. */
  get longPressAt(): number | null {
    return this.phase === "pending" ? this.startedAt + this.longPressMs : null;
  }

  get selecting(): boolean {
    return this.phase === "select";
  }

  cancel() {
    this.phase = "idle";
  }

  /** Call when a long-press timer fires (and the tracker calls it itself on move and end). */
  tick(now: number): GestureEvent[] {
    if (this.phase === "pending" && now - this.startedAt >= this.longPressMs) {
      this.phase = "select";
      return [{ type: "longpress", at: this.origin }];
    }
    return [];
  }

  /** `points` are all the fingers now on the screen. */
  start(points: Pt[], now: number): GestureEvent[] {
    if (points.length === 1 && (this.phase === "idle" || this.phase === "done")) {
      this.phase = "pending";
      this.startedAt = now;
      this.origin = points[0];
      this.last = points[0];
      return [];
    }
    if (points.length >= 2 && this.phase !== "select") {
      const [a, b] = points;
      this.phase = "two";
      this.startDistance = Math.max(1, distance(a, b));
      this.lastScale = 1;
      this.startMid = middle(a, b);
      this.last = this.startMid;
    }
    return [];
  }

  move(points: Pt[], now: number): GestureEvent[] {
    const events = this.tick(now);
    if (points.length === 0) return events;

    if (this.phase === "select") {
      this.last = points[0];
      return [...events, { type: "select-move", at: points[0] }];
    }
    if (points.length >= 2 && (this.phase === "pending" || this.phase === "scroll")) {
      // A second finger whose touchstart was not seen (it lands on another element).
      this.start(points, now);
    }
    if (this.phase === "pending" || this.phase === "scroll") {
      const point = points[0];
      if (this.phase === "pending" && distance(point, this.origin) < this.slop) return events;
      this.phase = "scroll";
      const dy = point.y - this.last.y;
      this.last = point;
      return dy === 0 ? events : [...events, { type: "scroll", fingers: 1, dy }];
    }
    if (points.length >= 2 && (this.phase === "two" || this.phase === "pinch" || this.phase === "scroll2")) {
      const [a, b] = points;
      const mid = middle(a, b);
      const scale = distance(a, b) / this.startDistance;
      if (this.phase === "two") {
        // Decide once: fingers moving apart or together is a pinch, moving together is a scroll.
        const spread = Math.abs(distance(a, b) - this.startDistance);
        const travel = distance(mid, this.startMid);
        if (spread >= this.slop && spread >= travel) this.phase = "pinch";
        else if (travel >= this.slop) this.phase = "scroll2";
        else return events;
      }
      if (this.phase === "pinch") {
        this.lastScale = scale;
        return [...events, { type: "pinch", scale }];
      }
      const dy = mid.y - this.last.y;
      this.last = mid;
      return dy === 0 ? events : [...events, { type: "scroll", fingers: 2, dy }];
    }
    return events;
  }

  /** `remaining` are the fingers still down after this one lifted. */
  end(remaining: Pt[], now: number): GestureEvent[] {
    const events = this.tick(now);
    const phase = this.phase;
    if (phase === "pending") {
      this.phase = remaining.length === 0 ? "idle" : "done";
      return [...events, { type: "tap", at: this.origin }];
    }
    if (phase === "select") {
      this.phase = "idle";
      return [...events, { type: "select-end", at: this.last }];
    }
    if (phase === "pinch") {
      this.phase = remaining.length === 0 ? "idle" : "done";
      return [...events, { type: "pinch-end", scale: this.lastScale }];
    }
    if (phase === "two" || phase === "scroll2" || phase === "scroll") {
      this.phase = remaining.length === 0 ? "idle" : "done";
      return events;
    }
    if (remaining.length === 0) this.phase = "idle";
    return events;
  }
}

/** Splits a pixel distance into whole lines, keeping the rest for the next move. */
export function takeLines(remainder: number, dy: number, lineHeight: number): { lines: number; remainder: number } {
  const total = remainder + dy;
  const lines = Math.trunc(total / lineHeight);
  return { lines, remainder: total - lines * lineHeight };
}

export const MIN_FONT = 8;
export const MAX_FONT = 32;

/** The font size after a pinch: the size at the start times the scale, rounded and kept in range. */
export function pinchFontSize(startSize: number, scale: number): number {
  return Math.min(MAX_FONT, Math.max(MIN_FONT, Math.round(startSize * scale)));
}

/** What a swipe sends in a full-screen program that has no scrollback and no
 *  mouse mode (less, man, vim): `lines` follows the content (positive: show
 *  earlier lines, as when the finger moves down). One finger sends cursor
 *  keys, two fingers send Page Up / Page Down for each `rows` lines. */
export function swipeKeys(
  lines: number,
  fingers: 1 | 2,
  applicationCursor: boolean,
  rows: number,
): string {
  if (lines === 0) return "";
  const up = lines > 0;
  const count = Math.abs(lines);
  if (fingers === 2) {
    const pages = Math.max(1, Math.round(count / Math.max(1, rows - 1)));
    return (up ? "\x1b[5~" : "\x1b[6~").repeat(pages);
  }
  const arrow = applicationCursor ? (up ? "\x1bOA" : "\x1bOB") : up ? "\x1b[A" : "\x1b[B";
  return arrow.repeat(count);
}
