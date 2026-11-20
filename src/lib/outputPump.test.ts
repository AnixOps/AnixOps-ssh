import { Terminal } from "@xterm/headless";
import { describe, expect, it } from "vitest";
import { OutputPump } from "./outputPump";

function manual() {
  const tasks: Array<() => void> = [];
  return {
    schedule: (run: () => void) => {
      tasks.push(run);
      return () => {
        const at = tasks.indexOf(run);
        if (at >= 0) tasks.splice(at, 1);
      };
    },
    tick: () => tasks.splice(0).forEach((run) => run()),
    count: () => tasks.length,
  };
}

describe("OutputPump", () => {
  it("joins many small events into one write per frame", () => {
    const clock = manual();
    const writes: string[] = [];
    const pump = new OutputPump({ write: (data, done) => (writes.push(data), done()) }, { schedule: clock.schedule });
    for (let i = 0; i < 1000; i++) pump.push("line\r\n");
    expect(writes).toHaveLength(0);
    clock.tick();
    expect(writes).toHaveLength(1);
    expect(writes[0]).toBe("line\r\n".repeat(1000));
    expect(pump.pending).toBe(0);
  });

  it("caps a frame and carries on in the next", () => {
    const clock = manual();
    const writes: string[] = [];
    const pump = new OutputPump({ write: (data, done) => (writes.push(data), done()) }, { schedule: clock.schedule, frameBudget: 10 });
    pump.push("a".repeat(25));
    clock.tick();
    clock.tick();
    clock.tick();
    expect(writes.map((w) => w.length)).toEqual([10, 10, 5]);
  });

  it("keeps surrogate pairs whole", () => {
    const clock = manual();
    const writes: string[] = [];
    const pump = new OutputPump({ write: (data, done) => (writes.push(data), done()) }, { schedule: clock.schedule, frameBudget: 3 });
    pump.push("ab😀cd");
    for (let i = 0; i < 4; i++) clock.tick();
    expect(writes.join("")).toBe("ab😀cd");
    for (const write of writes) expect(write).not.toMatch(/[\ud800-\udbff]$/);
  });

  it("holds output back above the high watermark until the parser drains below the low one", () => {
    const clock = manual();
    const acks: Array<() => void> = [];
    const writes: string[] = [];
    const pump = new OutputPump(
      { write: (data, done) => (writes.push(data), acks.push(done)) },
      { schedule: clock.schedule, frameBudget: 10, high: 20, low: 10 },
    );
    pump.push("x".repeat(100));
    clock.tick(); // 10 in flight
    clock.tick(); // 20 in flight: at the high mark
    expect(writes).toHaveLength(2);
    clock.tick();
    expect(writes).toHaveLength(2);
    expect(pump.paused).toBe(true);
    acks.shift()!(); // 10 in flight: not below low yet
    clock.tick();
    expect(writes).toHaveLength(2);
    acks.shift()!(); // 0 in flight
    clock.tick();
    expect(pump.paused).toBe(false);
    expect(writes.length).toBeGreaterThan(2);
  });

  it("drops what is queued when disposed", () => {
    const clock = manual();
    const writes: string[] = [];
    const pump = new OutputPump({ write: (data, done) => (writes.push(data), done()) }, { schedule: clock.schedule });
    pump.push("hello");
    pump.dispose();
    clock.tick();
    pump.push("again");
    clock.tick();
    expect(writes).toHaveLength(0);
    expect(clock.count()).toBe(0);
  });

  it("delivers every byte to a real terminal", async () => {
    const terminal = new Terminal({ cols: 80, rows: 5, scrollback: 100, allowProposedApi: true });
    const clock = manual();
    const pump = new OutputPump({ write: (data, done) => terminal.write(data, done) }, { schedule: clock.schedule, frameBudget: 1000 });
    for (let i = 0; i < 300; i++) pump.push(`row ${i}\r\n`);
    while (pump.pending > 0) {
      clock.tick();
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    const buffer = terminal.buffer.active;
    expect(buffer.getLine(buffer.baseY + 3)?.translateToString(true)).toBe("row 299");
  });
});
