import type { Offer, ZSession } from "zmodem.js";
import { describe, expect, it, vi } from "vitest";
import { TerminalTransfer, type TransferFiles } from "./terminalTransfer";

// The strings module reads the page's language when it loads; there is no page here.
vi.hoisted(() => {
  Object.assign(globalThis, { window: { location: { search: "?lang=en" } } });
});

/** A promise settled by hand. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function fakeFiles(overrides: Partial<TransferFiles>) {
  const closes: { id: number; keep: boolean }[] = [];
  const files: TransferFiles = {
    pickSend: async () => ["/tmp/a"],
    pickSave: async () => undefined,
    openRead: async () => ({ id: 1, name: "a", size: 0, modified: 0 }),
    read: async () => new Uint8Array(),
    openWrite: async () => ({ id: 1, path: "/tmp/a" }),
    write: async () => {},
    close: async (id, keep) => {
      closes.push({ id, keep });
    },
    ...overrides,
  };
  return { files, closes };
}

/** A transfer that believes a ZMODEM session is running, with the session replaced by a stub. */
function running(files: TransferFiles) {
  const transfer = new TerminalTransfer(files, () => ({ enabled: true, askWhere: false }) as never);
  const internals = transfer as unknown as { session: unknown; receive(offer: Offer): Promise<void>; send(session: ZSession): Promise<void> };
  const session = { abort: vi.fn(), close: vi.fn(async () => {}), send_offer: vi.fn(() => new Promise(() => {})) };
  internals.session = session;
  return { transfer, internals, session };
}

const offer = (accept: () => Promise<void>) =>
  ({ get_details: () => ({ name: "x.bin", size: 10, files_remaining: 0 }), accept, skip: vi.fn(), get_offset: () => 0 }) as unknown as Offer;

describe("a file opened after the transfer was cancelled", () => {
  it("is closed and removed exactly once when the receive side's open returns late", async () => {
    const open = deferred<{ id: number; path: string }>();
    const { files, closes } = fakeFiles({ openWrite: () => open.promise });
    const { transfer, internals } = running(files);
    const done = internals.receive(offer(() => new Promise(() => {})));
    await tick();
    transfer.cancel();
    await done;
    expect(closes).toEqual([]);
    open.resolve({ id: 7, path: "/tmp/x.bin.part" });
    await tick();
    expect(closes).toEqual([{ id: 7, keep: false }]);
    await tick();
    expect(closes).toHaveLength(1);
  });

  it("is closed exactly once when the send side's open returns late", async () => {
    const open = deferred<{ id: number; name: string; size: number; modified: number }>();
    const { files, closes } = fakeFiles({ openRead: () => open.promise });
    const { transfer, internals, session } = running(files);
    const done = internals.send(session as unknown as ZSession);
    await tick();
    transfer.cancel();
    await done;
    expect(closes).toEqual([]);
    open.resolve({ id: 9, name: "a", size: 1, modified: 0 });
    await tick();
    expect(closes).toEqual([{ id: 9, keep: false }]);
  });

  it("is not closed twice when the open finished before the cancel", async () => {
    const { files, closes } = fakeFiles({ openWrite: async () => ({ id: 3, path: "/tmp/p" }) });
    const { transfer, internals } = running(files);
    const done = internals.receive(offer(() => new Promise(() => {})));
    await tick();
    transfer.cancel();
    await done;
    await tick();
    expect(closes).toEqual([{ id: 3, keep: false }]);
  });

  it("send side: an open that finished before the cancel is closed once", async () => {
    const { files, closes } = fakeFiles({ openRead: async () => ({ id: 4, name: "a", size: 1, modified: 0 }) });
    const { transfer, internals, session } = running(files);
    const done = internals.send(session as unknown as ZSession);
    await tick();
    transfer.cancel();
    await done;
    await tick();
    expect(closes).toEqual([{ id: 4, keep: false }]);
  });

  it("a failing late disposal does not surface as an unhandled rejection", async () => {
    const open = deferred<{ id: number; path: string }>();
    const { files } = fakeFiles({
      openWrite: () => open.promise,
      close: async () => {
        throw new Error("gone");
      },
    });
    const { transfer, internals } = running(files);
    const done = internals.receive(offer(() => new Promise(() => {})));
    await tick();
    transfer.cancel();
    await done;
    open.resolve({ id: 1, path: "/p" });
    await tick();
  });
});
