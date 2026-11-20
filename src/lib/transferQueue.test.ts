import { describe, expect, it } from "vitest";
import {
  DEFAULT_TRANSFER_LIMIT,
  failureMessage,
  formatDuration,
  nextRate,
  pickToStart,
  secondsLeft,
  type QueueState,
} from "./transferQueue";

const items = (...states: QueueState[]) => states.map((state, index) => ({ id: `t${index}`, state }));

describe("pickToStart", () => {
  it("starts queued items in order up to the limit", () => {
    const twelve = items(...Array<QueueState>(12).fill("queued"));
    expect(pickToStart(twelve, DEFAULT_TRANSFER_LIMIT)).toEqual(["t0", "t1", "t2", "t3"]);
  });

  it("counts the running ones and ignores finished ones", () => {
    const list = items("running", "done", "failed", "queued", "running", "queued", "queued", "cancelled");
    expect(pickToStart(list, 4)).toEqual(["t3", "t5"]);
  });

  it("starts nothing when the limit is reached", () => {
    expect(pickToStart(items("running", "running", "queued"), 2)).toEqual([]);
  });

  it("honours a lower limit than the default", () => {
    expect(pickToStart(items("queued", "queued", "queued"), 1)).toEqual(["t0"]);
  });
});

describe("speed and time left", () => {
  it("has no speed from a single sample", () => {
    expect(nextRate(undefined, 0, 1000).perSecond).toBe(0);
  });

  it("measures bytes per second and smooths later samples", () => {
    const first = nextRate(undefined, 0, 0);
    const second = nextRate(first, 1000, 1000);
    expect(second.perSecond).toBe(1000);
    const third = nextRate(second, 1000 + 2000, 2000);
    expect(third.perSecond).toBeCloseTo(1000 * 0.7 + 2000 * 0.3);
  });

  it("estimates the time left only when it can", () => {
    expect(secondsLeft(100, 1100, 100)).toBe(10);
    expect(secondsLeft(100, null, 100)).toBeNull();
    expect(secondsLeft(100, 1100, 0)).toBeNull();
    expect(secondsLeft(2000, 1100, 100)).toBe(0);
  });

  it("formats durations", () => {
    expect(formatDuration(45)).toBe("45 s");
    expect(formatDuration(185)).toBe("3 min 5 s");
    expect(formatDuration(3720)).toBe("1 h 2 min");
  });
});

describe("failureMessage", () => {
  it("puts the remote path in front of the reason", () => {
    expect(failureMessage("/srv/a.txt", "Permission denied")).toBe("/srv/a.txt: Permission denied");
    expect(failureMessage("/srv/a.txt", new Error("timed out"))).toBe("/srv/a.txt: timed out");
  });

  it("does not repeat a path the message already names", () => {
    expect(failureMessage("/srv/a.txt", "writing /srv/a.txt: disk full")).toBe("writing /srv/a.txt: disk full");
  });

  it("gives a reason even when there is none", () => {
    expect(failureMessage("/x", "")).toBe("/x: unknown error");
  });
});
