import { describe, expect, it, vi } from "vitest";
import { MAX_PENDING, Sink, SinkOverflow, WRITE_BATCH, type TransferFiles } from "./terminalTransfer";

// The strings module reads the page's language when it loads; there is no page here.
vi.hoisted(() => {
  Object.assign(globalThis, { window: { location: { search: "?lang=en" } } });
});

/** A disk whose writes finish only when told to. */
function slowDisk(failAt = -1) {
  const written: Uint8Array[] = [];
  const gates: (() => void)[] = [];
  let calls = 0;
  const files = {
    write: (_id: number, bytes: Uint8Array) => {
      const call = calls++;
      return new Promise<void>((resolve, reject) => {
        gates.push(() => {
          if (call === failAt) reject(new Error("disk full"));
          else {
            written.push(bytes);
            resolve();
          }
        });
      });
    },
  } as unknown as TransferFiles;
  const release = async () => {
    for (;;) {
      await new Promise((r) => setTimeout(r, 0));
      const gate = gates.shift();
      if (!gate) return;
      gate();
    }
  };
  return { files, written, release, calls: () => calls };
}

/** Bytes whose value counts up, so order mistakes show. */
const pattern = (start: number, length: number) => Uint8Array.from({ length }, (_, i) => (start + i) & 0xff);

describe("received bytes waiting for a slow disk", () => {
  it("are written in batches of at most 1 MiB, in order, with a bounded backlog", async () => {
    const disk = slowDisk();
    const sink = new Sink(disk.files, 1);
    const chunk = 100_000;
    const total = 12 * 1024 * 1024; // under the cap, so the whole backlog queues behind the first write
    for (let at = 0; at < total; at += chunk) sink.push(pattern(at, Math.min(chunk, total - at)));
    expect(sink.error).toBeNull();
    expect(sink.peakPending).toBeLessThanOrEqual(MAX_PENDING);
    const flushed = sink.flush();
    await disk.release();
    await flushed;
    expect(disk.written.every((batch) => batch.length <= WRITE_BATCH)).toBe(true);
    const all = new Uint8Array(disk.written.reduce((sum, b) => sum + b.length, 0));
    let at = 0;
    for (const batch of disk.written) {
      all.set(batch, at);
      at += batch.length;
    }
    expect(all.length).toBe(total);
    for (let i = 0; i < total; i += 4099) expect(all[i]).toBe(i & 0xff);
    expect(all[total - 1]).toBe((total - 1) & 0xff);
  });

  it("stop at the cap: the sink fails with the buffer-full error and holds nothing more", async () => {
    const disk = slowDisk();
    const onError = vi.fn();
    const cap = 4 * 1024 * 1024;
    const sink = new Sink(disk.files, 1, onError, cap);
    const chunk = 256 * 1024;
    for (let i = 0; i < 200; i += 1) sink.push(pattern(0, chunk));
    expect(sink.peakPending).toBeLessThanOrEqual(cap);
    expect(onError).toHaveBeenCalledTimes(1);
    expect(onError.mock.calls[0][0]).toBeInstanceOf(SinkOverflow);
    const flushed = sink.flush().catch((e) => e);
    // Only the batch already in flight may still be written.
    await disk.release();
    expect(await flushed).toBeInstanceOf(SinkOverflow);
    expect(disk.calls()).toBe(1);
  });

  it("stop queueing after a write fails", async () => {
    const disk = slowDisk(0);
    const onError = vi.fn();
    const sink = new Sink(disk.files, 1, onError);
    sink.push(pattern(0, 1000));
    sink.push(pattern(1000, 1000));
    const flushed = sink.flush().catch((e) => e);
    await disk.release();
    expect(((await flushed) as Error).message).toBe("disk full");
    expect(onError).toHaveBeenCalledTimes(1);
    sink.push(pattern(0, 1000));
    await disk.release();
    expect(disk.calls()).toBe(1);
    expect(sink.peakPending).toBe(1000);
  });

  it("split a chunk larger than a batch without copying or reordering", async () => {
    const disk = slowDisk();
    const sink = new Sink(disk.files, 1);
    sink.push(pattern(0, 2 * WRITE_BATCH + 10));
    const flushed = sink.flush();
    await disk.release();
    await flushed;
    expect(disk.written.map((b) => b.length)).toEqual([WRITE_BATCH, WRITE_BATCH, 10]);
    expect(disk.written[2][0]).toBe((2 * WRITE_BATCH) & 0xff);
  });
});
