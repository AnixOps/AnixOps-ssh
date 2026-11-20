import { describe, expect, it, vi } from "vitest";
import {
  autoReconnectStep as step,
  backoffDelayMs,
  IDLE,
  isDropped,
  MAX_ATTEMPTS,
  secondsLeft,
  wokeFromSleep,
  type AutoReconnectState,
} from "./autoReconnect";

// terminalSession pulls in the strings module, which reads the page's language when it loads.
vi.hoisted(() => {
  Object.assign(globalThis, { window: { location: { search: "?lang=en" } } });
});

describe("backoff", () => {
  it("doubles from one second and stops at thirty", () => {
    expect([1, 2, 3, 4, 5, 6, 7, 8].map(backoffDelayMs)).toEqual([1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]);
    expect(backoffDelayMs(1000)).toBe(30000);
  });
});

describe("what counts as dropped", () => {
  it("retries lost connections only", () => {
    expect(isDropped({ code: null })).toBe(true);
    expect(isDropped({ code: null, reason: "disconnected" })).toBe(true);
    expect(isDropped({ code: 0, reason: "exit" })).toBe(false);
    expect(isDropped({ code: 1 })).toBe(false);
    expect(isDropped({ code: null, reason: "signal", signal: "TERM" })).toBe(false);
  });
});

describe("state machine", () => {
  it("waits 1 s after a drop, then connects", () => {
    let state = step(IDLE, { type: "ended", eligible: true, now: 100 });
    expect(state).toEqual({ kind: "waiting", attempt: 1, dueAt: 1100 });
    state = step(state, { type: "fire" });
    expect(state).toEqual({ kind: "connecting", attempt: 1 });
    expect(step(state, { type: "connected" })).toEqual(IDLE);
  });

  it("ignores an ineligible end", () => {
    expect(step(IDLE, { type: "ended", eligible: false, now: 0 })).toEqual(IDLE);
  });

  it("backs off after each failed attempt and gives up after ten", () => {
    let state: AutoReconnectState = step(IDLE, { type: "ended", eligible: true, now: 0 });
    const waits: number[] = [];
    let now = 0;
    for (let i = 0; i < MAX_ATTEMPTS; i++) {
      if (state.kind !== "waiting") break;
      waits.push(state.dueAt - now);
      now = state.dueAt;
      state = step(state, { type: "fire" });
      state = step(state, { type: "ended", eligible: true, now });
    }
    expect(waits).toEqual([1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000, 30000, 30000]);
    expect(state).toEqual({ kind: "stopped", reason: "exhausted" });
  });

  it("a failed attempt with the setting off stops quietly", () => {
    const state = step({ kind: "connecting", attempt: 2 }, { type: "ended", eligible: false, now: 0 });
    expect(state).toEqual(IDLE);
  });

  it("the network coming back skips the wait", () => {
    const waiting = step(IDLE, { type: "ended", eligible: true, now: 0 });
    const state = step({ kind: "waiting", attempt: 3, dueAt: 9000 }, { type: "network", now: 500 });
    expect(state).toEqual({ kind: "waiting", attempt: 3, dueAt: 500 });
    expect(step(waiting, { type: "network", now: 5000 })).toEqual(waiting);
  });

  it("the network restarts after giving up but not after cancel or interaction", () => {
    const exhausted: AutoReconnectState = { kind: "stopped", reason: "exhausted" };
    expect(step(exhausted, { type: "network", now: 7 })).toEqual({ kind: "waiting", attempt: 1, dueAt: 7 });
    for (const reason of ["cancelled", "interaction"] as const) {
      const stopped: AutoReconnectState = { kind: "stopped", reason };
      expect(step(stopped, { type: "network", now: 7 })).toBe(stopped);
    }
  });

  it("interaction stops only a running attempt", () => {
    expect(step({ kind: "connecting", attempt: 2 }, { type: "interaction" })).toEqual({
      kind: "stopped",
      reason: "interaction",
    });
    const waiting: AutoReconnectState = { kind: "waiting", attempt: 2, dueAt: 5 };
    expect(step(waiting, { type: "interaction" })).toBe(waiting);
  });

  it("cancel stops, reset clears", () => {
    const cancelled = step({ kind: "waiting", attempt: 2, dueAt: 5 }, { type: "cancel" });
    expect(cancelled).toEqual({ kind: "stopped", reason: "cancelled" });
    expect(step(cancelled, { type: "ended", eligible: true, now: 0 })).toEqual(IDLE);
    expect(step(cancelled, { type: "reset" })).toEqual(IDLE);
    expect(step(cancelled, { type: "connected" })).toBe(cancelled);
  });

  it("fire outside waiting does nothing", () => {
    expect(step(IDLE, { type: "fire" })).toEqual(IDLE);
  });
});

describe("helpers", () => {
  it("rounds the countdown up and never shows zero", () => {
    expect(secondsLeft(3000, 0)).toBe(3);
    expect(secondsLeft(2100, 0)).toBe(3);
    expect(secondsLeft(100, 0)).toBe(1);
    expect(secondsLeft(0, 500)).toBe(1);
  });

  it("notices a sleep gap", () => {
    expect(wokeFromSleep(0, 5000)).toBe(false);
    expect(wokeFromSleep(0, 60000)).toBe(true);
  });
});
