import { describe, expect, it } from "vitest";
import { BackgroundPolicy, PROBE_AFTER_MS, resumeAction, type ResumeInfo, type Timers } from "./backgroundPolicy";

/** A clock and timers the test moves by hand. */
function fakeTimers() {
  let time = 0;
  let nextHandle = 1;
  const pending = new Map<number, { at: number; callback: () => void }>();
  const timers: Timers = {
    now: () => time,
    setTimeout: (callback, ms) => {
      const handle = nextHandle++;
      pending.set(handle, { at: time + ms, callback });
      return handle;
    },
    clearTimeout: (handle) => void pending.delete(handle as number),
  };
  return {
    timers,
    advance(ms: number) {
      const target = time + ms;
      for (;;) {
        const due = [...pending.entries()].filter(([, timer]) => timer.at <= target).sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        pending.delete(due[0]);
        time = due[1].at;
        due[1].callback();
      }
      time = target;
    },
    pending: () => pending.size,
  };
}

function rig(graceSeconds: number) {
  const clock = fakeTimers();
  const log: string[] = [];
  const resumes: ResumeInfo[] = [];
  const policy = new BackgroundPolicy(
    graceSeconds,
    {
      onExpire: () => log.push("expire"),
      onResume: (info) => {
        log.push("resume");
        resumes.push(info);
      },
    },
    clock.timers,
  );
  return { clock, policy, log, resumes };
}

describe("grace period", () => {
  it("keeps the connection through a short trip to the background", () => {
    const { clock, policy, log, resumes } = rig(60);
    policy.hidden();
    clock.advance(20_000);
    policy.visible();
    expect(log).toEqual(["resume"]);
    expect(resumes[0]).toEqual({ awayMs: 20_000, expired: false });
    expect(clock.pending()).toBe(0);
  });

  it("closes the connection when the grace period runs out, once", () => {
    const { clock, policy, log, resumes } = rig(60);
    policy.hidden();
    clock.advance(59_999);
    expect(log).toEqual([]);
    clock.advance(1);
    expect(log).toEqual(["expire"]);
    clock.advance(600_000);
    expect(log).toEqual(["expire"]);
    policy.visible();
    expect(resumes[0]).toEqual({ awayMs: 659_999 + 1, expired: true });
  });

  it("zero closes at once and a negative value never closes", () => {
    const now = rig(0);
    now.policy.hidden();
    now.clock.advance(0);
    expect(now.log).toEqual(["expire"]);

    const never = rig(-1);
    never.policy.hidden();
    never.clock.advance(24 * 3600_000);
    expect(never.log).toEqual([]);
    never.policy.visible();
    expect(never.resumes[0].expired).toBe(false);
  });

  it("a new trip gets a fresh period", () => {
    const { clock, policy, log } = rig(30);
    policy.hidden();
    clock.advance(20_000);
    policy.visible();
    policy.hidden();
    clock.advance(20_000);
    expect(log).toEqual(["resume"]);
    clock.advance(10_000);
    expect(log).toEqual(["resume", "expire"]);
  });

  it("ignores repeated hidden and visible events", () => {
    const { clock, policy, log } = rig(30);
    policy.visible();
    policy.hidden();
    clock.advance(10_000);
    policy.hidden();
    clock.advance(20_000);
    expect(log).toEqual(["expire"]);
    policy.visible();
    policy.visible();
    expect(log).toEqual(["expire", "resume"]);
  });

  it("changing the period while hidden counts the time already spent", () => {
    const { clock, policy, log } = rig(300);
    policy.hidden();
    clock.advance(100_000);
    policy.setGrace(120);
    clock.advance(19_999);
    expect(log).toEqual([]);
    clock.advance(1);
    expect(log).toEqual(["expire"]);
  });

  it("disposing stops the timer", () => {
    const { clock, policy, log } = rig(30);
    policy.hidden();
    policy.dispose();
    clock.advance(60_000);
    expect(log).toEqual([]);
    expect(clock.pending()).toBe(0);
  });
});

describe("what to do on return", () => {
  const away = (awayMs: number, expired = false): ResumeInfo => ({ awayMs, expired });

  it("leaves a fresh connection alone", () => {
    expect(resumeAction(away(3_000), true, true)).toBe("none");
    expect(resumeAction(away(3_000), false, true)).toBe("none");
  });

  it("replaces an attached connection after a while, because the socket may be dead", () => {
    expect(resumeAction(away(PROBE_AFTER_MS - 1), true, true)).toBe("none");
    expect(resumeAction(away(PROBE_AFTER_MS), true, true)).toBe("reconnect");
  });

  it("does not replace the shell of a plain session by itself", () => {
    expect(resumeAction(away(120_000), false, true)).toBe("none");
  });

  it("after the grace period an attached session reconnects and a plain one asks", () => {
    expect(resumeAction(away(90_000, true), true, false)).toBe("reconnect");
    expect(resumeAction(away(90_000, true), false, false)).toBe("offer");
  });

  it("a session that ended in the background is handled the same way", () => {
    expect(resumeAction(away(5_000), true, false)).toBe("reconnect");
    expect(resumeAction(away(5_000), false, false)).toBe("offer");
  });
});
