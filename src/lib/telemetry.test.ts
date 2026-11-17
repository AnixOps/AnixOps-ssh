import { describe, expect, it, vi } from "vitest";
import {
  MAX_PAGE_REPORTS,
  installErrorReporting,
  needsFirstRun,
  pendingText,
  privacyView,
  type TelemetryStatus,
} from "./telemetry";

const can: TelemetryStatus = { crashAvailable: true, usageAvailable: true, pending: 0 };
const cannot: TelemetryStatus = { crashAvailable: false, usageAvailable: false, pending: 0 };

describe("first-run question", () => {
  it("is asked only when nothing was answered and the build can send", () => {
    expect(needsFirstRun({ crashReports: null, usageStats: false }, can)).toBe(true);
    expect(needsFirstRun({ crashReports: true, usageStats: false }, can)).toBe(false);
    expect(needsFirstRun({ crashReports: false, usageStats: false }, can)).toBe(false);
  });

  it("is not asked by a build without a server, or before the status is known", () => {
    expect(needsFirstRun({ crashReports: null, usageStats: false }, cannot)).toBe(false);
    expect(needsFirstRun({ crashReports: null, usageStats: false }, null)).toBe(false);
  });
});

describe("privacy settings view", () => {
  it("shows an unanswered question as off", () => {
    const view = privacyView({ crashReports: null, usageStats: false }, can);
    expect(view.crashOn).toBe(false);
    expect(view.usageOn).toBe(false);
  });

  it("keeps usage statistics independent and off by default", () => {
    const view = privacyView({ crashReports: true, usageStats: false }, can);
    expect(view.crashOn).toBe(true);
    expect(view.usageOn).toBe(false);
  });

  it("says plainly when the build collects nothing", () => {
    const view = privacyView({ crashReports: true, usageStats: true }, cannot);
    expect(view.nothingCollected).toBe(true);
    expect(view.crashOn).toBe(false);
    expect(view.usageOn).toBe(false);
  });

  it("can offer crash reports without usage statistics", () => {
    const view = privacyView({ crashReports: true, usageStats: true }, { ...can, usageAvailable: false });
    expect(view.nothingCollected).toBe(false);
    expect(view.usageOn).toBe(false);
  });
});

describe("pending reports view", () => {
  it("shows exactly the text that would be sent", () => {
    const text = pendingText({
      reports: [{ id: "a", text: '{"event_id":"a"}\n{"type":"event"}\n{}\n' }],
      usage: '{"os":"linux"}',
    });
    expect(text).toBe('{"event_id":"a"}\n{"type":"event"}\n{}\n\n{"os":"linux"}');
  });

  it("is empty when there is nothing", () => {
    expect(pendingText({ reports: [], usage: null })).toBe("");
  });
});

describe("frontend error reporting", () => {
  function target() {
    const handlers = new Map<string, (event: Event) => void>();
    return {
      addEventListener: (type: string, fn: (event: Event) => void) => void handlers.set(type, fn),
      fire: (type: string, event: object) => handlers.get(type)?.(event as Event),
    };
  }

  it("reports window errors and unhandled rejections", () => {
    const t = target();
    const report = vi.fn();
    installErrorReporting(t, report);
    t.fire("error", { message: "boom", error: { message: "boom", stack: "at f (x:1:1)" } });
    t.fire("unhandledrejection", { reason: new Error("nope") });
    t.fire("unhandledrejection", { reason: "plain" });
    expect(report).toHaveBeenNthCalledWith(1, "error", "boom", "at f (x:1:1)");
    expect(report.mock.calls[1][0]).toBe("unhandledrejection");
    expect(report.mock.calls[1][1]).toBe("nope");
    expect(report).toHaveBeenNthCalledWith(3, "unhandledrejection", "plain", "");
  });

  it("does not repeat a message and stops after a few", () => {
    const t = target();
    const report = vi.fn();
    installErrorReporting(t, report);
    for (let i = 0; i < 3; i++) t.fire("error", { message: "same" });
    expect(report).toHaveBeenCalledTimes(1);
    for (let i = 0; i < 20; i++) t.fire("error", { message: `m${i}` });
    expect(report).toHaveBeenCalledTimes(MAX_PAGE_REPORTS);
  });

  it("never raises from inside the handler", () => {
    const t = target();
    installErrorReporting(t, () => {
      throw new Error("backend down");
    });
    expect(() => t.fire("error", { message: "x" })).not.toThrow();
    const rejecting = target();
    installErrorReporting(rejecting, () => Promise.reject(new Error("down")));
    expect(() => rejecting.fire("error", { message: "y" })).not.toThrow();
  });
});
