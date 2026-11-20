import { describe, expect, it, vi } from "vitest";
import { OutputBuffer } from "./outputBuffer";

function setup(extra: Partial<ConstructorParameters<typeof OutputBuffer>[0]> = {}) {
  const writes: { data: string; done: () => void }[] = [];
  let frame: (() => void) | null = null;
  const onPause = vi.fn();
  const onResume = vi.fn();
  const buffer = new OutputBuffer({
    write: (data, done) => writes.push({ data, done }),
    schedule: (flush) => {
      frame = flush;
      return () => (frame = null);
    },
    highWater: 100,
    lowWater: 40,
    onPause,
    onResume,
    ...extra,
  });
  return { buffer, writes, onPause, onResume, runFrame: () => frame?.() };
}

describe("OutputBuffer", () => {
  it("joins what arrives between frames into one write", () => {
    const { buffer, writes, runFrame } = setup();
    buffer.push("a");
    buffer.push("b");
    buffer.push("c");
    expect(writes).toHaveLength(0);
    runFrame();
    expect(writes.map((write) => write.data)).toEqual(["abc"]);
    buffer.push("d");
    runFrame();
    expect(writes.map((write) => write.data)).toEqual(["abc", "d"]);
  });

  it("counts queued and in-flight characters", () => {
    const { buffer, writes, runFrame } = setup();
    buffer.push("12345");
    expect(buffer.pending).toBe(5);
    runFrame();
    expect(buffer.pending).toBe(5);
    writes[0].done();
    expect(buffer.pending).toBe(0);
  });

  it("pauses above the high mark and resumes below the low mark", () => {
    const { buffer, writes, onPause, onResume, runFrame } = setup();
    buffer.push("x".repeat(60));
    expect(onPause).not.toHaveBeenCalled();
    buffer.push("y".repeat(60));
    expect(onPause).toHaveBeenCalledTimes(1);
    expect(buffer.isPaused).toBe(true);
    buffer.push("z".repeat(10));
    expect(onPause).toHaveBeenCalledTimes(1);
    runFrame();
    writes[0].done();
    expect(onResume).toHaveBeenCalledTimes(1);
    expect(buffer.isPaused).toBe(false);
  });

  it("stays paused while more than the low mark is pending", () => {
    const { buffer, writes, onResume, runFrame } = setup();
    buffer.push("x".repeat(110));
    runFrame();
    buffer.push("y".repeat(50));
    runFrame();
    writes[0].done();
    expect(onResume).not.toHaveBeenCalled();
    writes[1].done();
    expect(onResume).toHaveBeenCalledTimes(1);
  });

  it("ignores data after dispose and cancels the frame", () => {
    const { buffer, writes, runFrame } = setup();
    buffer.push("a");
    buffer.dispose();
    runFrame();
    buffer.push("b");
    buffer.flush();
    expect(writes).toHaveLength(0);
  });
});
