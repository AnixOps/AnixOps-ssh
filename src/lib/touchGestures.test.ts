import { describe, expect, it } from "vitest";
import {
  GestureTracker,
  LONG_PRESS_MS,
  MAX_FONT,
  MIN_FONT,
  pinchFontSize,
  swipeKeys,
  takeLines,
  type GestureEvent,
  type Pt,
} from "./touchGestures";

const p = (x: number, y: number): Pt => ({ x, y });

/** Replays touches and collects the events. */
function run(steps: (tracker: GestureTracker, out: GestureEvent[]) => void): GestureEvent[] {
  const tracker = new GestureTracker();
  const out: GestureEvent[] = [];
  steps(tracker, out);
  return out;
}

describe("one finger", () => {
  it("a quick touch is a tap", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(100, 100)], 0));
      out.push(...g.end([], 80));
    });
    expect(events).toEqual([{ type: "tap", at: p(100, 100) }]);
  });

  it("a touch that wobbles under the slop is still a tap", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(100, 100)], 0));
      out.push(...g.move([p(104, 103)], 40));
      out.push(...g.end([], 90));
    });
    expect(events).toEqual([{ type: "tap", at: p(100, 100) }]);
  });

  it("dragging past the slop scrolls, by the distance moved since the last event", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(100, 300)], 0));
      out.push(...g.move([p(100, 295)], 16)); // under the slop: nothing yet
      out.push(...g.move([p(100, 280)], 32));
      out.push(...g.move([p(100, 250)], 48));
      out.push(...g.end([], 60));
    });
    expect(events).toEqual([
      { type: "scroll", fingers: 1, dy: -20 },
      { type: "scroll", fingers: 1, dy: -30 },
    ]);
  });

  it("a scroll never ends in a tap or a long press", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(100, 300)], 0));
      out.push(...g.move([p(100, 200)], 100));
      out.push(...g.tick(2000));
      out.push(...g.end([], 2100));
    });
    expect(events.map((event) => event.type)).toEqual(["scroll"]);
  });

  it("holding still selects (long press), and the same touch can extend the selection", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(50, 60)], 0));
      expect(g.longPressAt).toBe(LONG_PRESS_MS);
      out.push(...g.tick(LONG_PRESS_MS));
      expect(g.selecting).toBe(true);
      out.push(...g.move([p(80, 60)], 600));
      out.push(...g.move([p(120, 90)], 620));
      out.push(...g.end([], 700));
    });
    expect(events).toEqual([
      { type: "longpress", at: p(50, 60) },
      { type: "select-move", at: p(80, 60) },
      { type: "select-move", at: p(120, 90) },
      { type: "select-end", at: p(120, 90) },
    ]);
  });

  it("the timer is not needed: a late move or release also triggers the long press", () => {
    const late = run((g, out) => {
      out.push(...g.start([p(50, 60)], 0));
      out.push(...g.end([], 800));
    });
    expect(late.map((event) => event.type)).toEqual(["longpress", "select-end"]);
  });

  it("moving before the time cancels the long press", () => {
    const g = new GestureTracker();
    g.start([p(50, 60)], 0);
    g.move([p(50, 100)], 100);
    expect(g.longPressAt).toBeNull();
    expect(g.tick(1000)).toEqual([]);
  });

  it("a release just before the time is a tap", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(50, 60)], 0));
      out.push(...g.end([], LONG_PRESS_MS - 1));
    });
    expect(events.map((event) => event.type)).toEqual(["tap"]);
  });
});

describe("two fingers", () => {
  it("moving apart is a pinch with the ratio of the distances", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(100, 300)], 0));
      out.push(...g.start([p(100, 300), p(200, 300)], 20));
      out.push(...g.move([p(90, 300), p(210, 300)], 40)); // 100 -> 120
      out.push(...g.move([p(50, 300), p(250, 300)], 60)); // 200
      out.push(...g.end([p(250, 300)], 80));
      out.push(...g.end([], 90));
    });
    expect(events).toEqual([
      { type: "pinch", scale: 1.2 },
      { type: "pinch", scale: 2 },
      { type: "pinch-end", scale: 2 },
    ]);
  });

  it("moving together is a two-finger scroll, not a pinch", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(100, 300)], 0));
      out.push(...g.start([p(100, 300), p(200, 300)], 20));
      out.push(...g.move([p(100, 280), p(200, 280)], 40)); // travel 20 > slop, spread 0
      out.push(...g.move([p(100, 240), p(200, 240)], 60));
      out.push(...g.end([p(200, 240)], 80));
      out.push(...g.end([], 90));
    });
    expect(events).toEqual([
      { type: "scroll", fingers: 2, dy: -20 },
      { type: "scroll", fingers: 2, dy: -40 },
    ]);
  });

  it("two fingers do not tap, and a finger left down afterwards does nothing", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(100, 300)], 0));
      out.push(...g.start([p(100, 300), p(200, 300)], 10));
      out.push(...g.end([p(200, 300)], 50));
      out.push(...g.move([p(200, 100)], 80));
      out.push(...g.end([], 120));
    });
    expect(events).toEqual([]);
  });

  it("a second finger cancels a pending long press", () => {
    const g = new GestureTracker();
    g.start([p(100, 300)], 0);
    g.start([p(100, 300), p(180, 300)], 100);
    expect(g.longPressAt).toBeNull();
    expect(g.tick(2000)).toEqual([]);
  });

  it("a second finger during a selection drag is ignored", () => {
    const events = run((g, out) => {
      out.push(...g.start([p(50, 60)], 0));
      out.push(...g.tick(500));
      out.push(...g.start([p(50, 60), p(300, 60)], 520));
      out.push(...g.move([p(60, 60), p(300, 60)], 540));
    });
    expect(events.map((event) => event.type)).toEqual(["longpress", "select-move"]);
  });

  it("the tracker is reusable after a gesture", () => {
    const g = new GestureTracker();
    g.start([p(1, 1)], 0);
    g.end([], 10);
    g.start([p(5, 5)], 1000);
    expect(g.end([], 1010)).toEqual([{ type: "tap", at: p(5, 5) }]);
  });
});

describe("lines and font size", () => {
  it("turns pixels into whole lines and keeps the remainder", () => {
    expect(takeLines(0, 25, 20)).toEqual({ lines: 1, remainder: 5 });
    expect(takeLines(5, 20, 20)).toEqual({ lines: 1, remainder: 5 });
    expect(takeLines(5, -50, 20)).toEqual({ lines: -2, remainder: -5 });
    expect(takeLines(0, 3, 20)).toEqual({ lines: 0, remainder: 3 });
  });

  it("scales the font by the pinch and keeps it in range", () => {
    expect(pinchFontSize(14, 1.5)).toBe(21);
    expect(pinchFontSize(14, 0.5)).toBe(MIN_FONT);
    expect(pinchFontSize(14, 10)).toBe(MAX_FONT);
    expect(pinchFontSize(14, 1.03)).toBe(14);
  });
});

describe("swiping inside a full-screen program", () => {
  it("one finger sends cursor keys that follow the finger", () => {
    expect(swipeKeys(3, 1, false, 24)).toBe("\x1b[A".repeat(3));
    expect(swipeKeys(-2, 1, false, 24)).toBe("\x1b[B".repeat(2));
    expect(swipeKeys(1, 1, true, 24)).toBe("\x1bOA");
    expect(swipeKeys(-1, 1, true, 24)).toBe("\x1bOB");
    expect(swipeKeys(0, 1, false, 24)).toBe("");
  });

  it("two fingers send a page key for each screenful, at least one", () => {
    expect(swipeKeys(5, 2, false, 24)).toBe("\x1b[5~");
    expect(swipeKeys(-5, 2, false, 24)).toBe("\x1b[6~");
    expect(swipeKeys(46, 2, false, 24)).toBe("\x1b[5~".repeat(2));
  });
});
