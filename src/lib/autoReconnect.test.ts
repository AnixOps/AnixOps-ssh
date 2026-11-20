import { describe, expect, it, vi } from "vitest";
import {
  RECONNECT_GIVE_UP_MS,
  RECONNECT_MAX_ATTEMPTS,
  RECONNECT_MAX_DELAY_MS,
  backoffDelay,
  beginRetry,
  classifyFailure,
  decideRetry,
  nextRetry,
  nudgeRetry,
  retryNow,
  secondsLeft,
  type EndContext,
  type RetryPlan,
} from "./autoReconnect";

// The strings module (via terminalSession) reads the page language when it loads.
vi.hoisted(() => {
  Object.assign(globalThis, { window: { location: { search: "?lang=en" } } });
});

const ctx: EndContext = { enabled: true, remote: true, wasConnected: true, prompted: false };

describe("backoffDelay", () => {
  it("doubles from one second and stops at the cap", () => {
    expect([1, 2, 3, 4, 5, 6, 7, 50].map(backoffDelay)).toEqual([1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]);
    expect(backoffDelay(0)).toBe(1000);
  });
});

describe("retry plan", () => {
  it("starts with the first delay", () => {
    expect(beginRetry(10_000)).toEqual({ attempt: 1, dueAt: 11_000, startedAt: 10_000 });
  });

  it("walks the backoff and keeps the start", () => {
    let plan: RetryPlan | null = beginRetry(0);
    const delays: number[] = [];
    let now = 0;
    for (let i = 0; i < 4 && plan; i++) {
      now = plan.dueAt;
      plan = nextRetry(plan, now);
      if (plan) delays.push(plan.dueAt - now);
    }
    expect(delays).toEqual([2000, 4000, 8000, 16000]);
    expect(plan?.startedAt).toBe(0);
  });

  it("gives up after five minutes", () => {
    const plan = beginRetry(0);
    expect(nextRetry(plan, RECONNECT_GIVE_UP_MS - 1)).not.toBeNull();
    expect(nextRetry(plan, RECONNECT_GIVE_UP_MS)).toBeNull();
  });

  it("gives up after the attempt limit", () => {
    const plan = { attempt: RECONNECT_MAX_ATTEMPTS, dueAt: 0, startedAt: 0 };
    expect(nextRetry(plan, 1)).toBeNull();
    expect(nextRetry({ ...plan, attempt: RECONNECT_MAX_ATTEMPTS - 1 }, 1)?.attempt).toBe(RECONNECT_MAX_ATTEMPTS);
  });

  it("never waits more than the cap", () => {
    const next = nextRetry({ attempt: 10, dueAt: 0, startedAt: 0 }, 1000);
    expect(next && next.dueAt - 1000).toBe(RECONNECT_MAX_DELAY_MS);
  });

  it("retries at once when the network is back, with fresh backoff", () => {
    expect(nudgeRetry(5000)).toEqual({ attempt: 1, dueAt: 5000, startedAt: 5000 });
  });

  it("brings the wait forward without resetting the count", () => {
    expect(retryNow({ attempt: 6, dueAt: 90_000, startedAt: 1 }, 5000)).toEqual({ attempt: 6, dueAt: 5000, startedAt: 1 });
    expect(retryNow({ attempt: 2, dueAt: 3000, startedAt: 1 }, 5000).dueAt).toBe(3000);
  });

  it("counts the seconds left, rounded up", () => {
    const plan = { attempt: 1, dueAt: 4000, startedAt: 0 };
    expect(secondsLeft(plan, 0)).toBe(4);
    expect(secondsLeft(plan, 3001)).toBe(1);
    expect(secondsLeft(plan, 9000)).toBe(0);
  });
});

describe("classifyFailure", () => {
  it("retries only network trouble", () => {
    for (const m of ["connection timed out", "Connection refused (os error 111)", "no route to host", "failed to resolve host", "network is unreachable", "Connection reset by peer"])
      expect(classifyFailure(m)).toBe("network");
  });

  it("keeps the user's decisions apart", () => {
    expect(classifyFailure("Permission denied (publickey,password)")).toBe("auth");
    expect(classifyFailure("authentication failed")).toBe("auth");
    expect(classifyFailure("host key verification failed")).toBe("hostkey");
    expect(classifyFailure("REMOTE HOST IDENTIFICATION HAS CHANGED: fingerprint")).toBe("hostkey");
    expect(classifyFailure("the vault is locked")).toBe("vault");
    expect(classifyFailure("cancelled by the user")).toBe("cancelled");
    expect(classifyFailure("something odd")).toBe("other");
  });
});

describe("decideRetry", () => {
  it("retries a lost connection of a session that was open", () => {
    expect(decideRetry({ code: null, reason: "disconnected" }, ctx)).toEqual({ retry: true });
    expect(decideRetry({ code: null }, ctx)).toEqual({ retry: true });
  });

  it("does not retry a clean exit, even a non-zero one", () => {
    expect(decideRetry({ code: 0, reason: "exit" }, ctx)).toEqual({ retry: false, why: "clean" });
    expect(decideRetry({ code: 255, reason: "exit" }, ctx)).toEqual({ retry: false, why: "clean" });
  });

  it("does not retry a kill by signal", () => {
    expect(decideRetry({ code: null, reason: "signal", signal: "KILL" }, ctx)).toEqual({ retry: false, why: "signal" });
  });

  it("does not retry auth, host key or vault failures", () => {
    const fail = (error: string) => decideRetry({ code: null, error }, ctx);
    expect(fail("Permission denied (publickey)")).toEqual({ retry: false, why: "auth" });
    expect(fail("host key mismatch")).toEqual({ retry: false, why: "hostkey" });
    expect(fail("the vault is locked")).toEqual({ retry: false, why: "vault" });
  });

  it("keeps trying while a retry chain fails on the network", () => {
    expect(decideRetry({ code: null, error: "connection timed out" }, ctx)).toEqual({ retry: true });
  });

  it("does not start a chain for a session that never opened", () => {
    const never = { ...ctx, wasConnected: false };
    expect(decideRetry({ code: null, reason: "disconnected" }, never)).toEqual({ retry: false, why: "never-connected" });
    expect(decideRetry({ code: null, error: "connection refused" }, never)).toEqual({ retry: false, why: "never-connected" });
  });

  it("falls back to the manual prompt when credentials were asked for", () => {
    expect(decideRetry({ code: null, reason: "disconnected" }, { ...ctx, prompted: true })).toEqual({ retry: false, why: "prompted" });
  });

  it("respects the setting and local shells", () => {
    expect(decideRetry({ code: null }, { ...ctx, enabled: false })).toEqual({ retry: false, why: "disabled" });
    expect(decideRetry({ code: null }, { ...ctx, remote: false })).toEqual({ retry: false, why: "local" });
  });
});
