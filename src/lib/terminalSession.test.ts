import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { t } from "./i18n";
import { exitReason, exitTitle, TERMINAL_RESET, trailingDebounce } from "./terminalSession";

// The strings module reads the page's language when it loads; there is no page here.
vi.hoisted(() => {
  Object.assign(globalThis, { window: { location: { search: "?lang=en" } } });
});

describe("exitTitle", () => {
  it("names the exit code", () => {
    expect(exitTitle({ code: 3, reason: "exit" })).toBe(t.sessionEndedCode.replace("{code}", "3"));
    expect(exitTitle({ code: 0, reason: "exit" })).toContain("0");
  });

  it("names the signal with its SIG prefix", () => {
    const title = exitTitle({ code: null, reason: "signal", signal: "KILL" });
    expect(title).toBe(t.sessionKilled.replace("{signal}", "SIGKILL"));
    expect(exitTitle({ code: null, reason: "signal", signal: "SIGTERM" })).toContain("SIGTERM");
    expect(exitTitle({ code: null, reason: "signal", signal: "SIGTERM" })).not.toContain("SIGSIG");
  });

  it("says the connection was lost when there is no status", () => {
    expect(exitTitle({ code: null, reason: "disconnected" })).toBe(t.connectionLost);
  });

  it("reads an older backend's event from the code alone", () => {
    expect(exitReason({ code: 1 })).toBe("exit");
    expect(exitReason({ code: null })).toBe("disconnected");
    expect(exitTitle({ code: null })).toBe(t.connectionLost);
  });
});

describe("trailingDebounce", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("calls once with the last arguments after the wait", () => {
    const calls: [number, number][] = [];
    const resize = trailingDebounce((cols: number, rows: number) => calls.push([cols, rows]), 50);
    resize(80, 24);
    vi.advanceTimersByTime(30);
    resize(100, 30);
    resize(120, 40);
    vi.advanceTimersByTime(49);
    expect(calls).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(calls).toEqual([[120, 40]]);
    resize(90, 20);
    vi.advanceTimersByTime(50);
    expect(calls).toEqual([
      [120, 40],
      [90, 20],
    ]);
  });

  it("drops a pending call when cancelled", () => {
    const fn = vi.fn();
    const debounced = trailingDebounce(fn, 50);
    debounced();
    debounced.cancel();
    vi.advanceTimersByTime(100);
    expect(fn).not.toHaveBeenCalled();
  });
});

describe("TERMINAL_RESET", () => {
  it("leaves the alternate screen and turns off mouse reporting and bracketed paste", () => {
    for (const sequence of ["\x1b[?1049l", "\x1b[!p", "\x1b[?1000l", "\x1b[?1006l", "\x1b[?2004l", "\x1b[?1004l"]) {
      expect(TERMINAL_RESET).toContain(sequence);
    }
  });
});
