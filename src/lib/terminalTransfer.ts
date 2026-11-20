import Zmodem, { type Detection, type Offer, type Sentry, type ZSession } from "zmodem.js";
import { t } from "./i18n";
import { bytesToText, displayText, textToBytes, ZMODEM_LOOKBEHIND, ZMODEM_START } from "./terminalBytes";
import type { TransferPrefs } from "./terminalTransferPrefs";

/* rz/sz in the terminal: ZMODEM, as Xshell, FinalShell and WindTerm do it.

   The session's output passes through `feed`. Ordinary output goes straight on
   to the terminal. When the ZMODEM handshake of `sz` (ZRQINIT) or `rz`
   (ZRINIT) shows up, the stream is handed to zmodem.js, nothing more is
   written to the terminal, and the transfer runs against local files through
   `TransferFiles`. When it ends, output flows to the terminal again.

   Bytes: the engine delivers terminal bytes as text without loss (see
   terminalBytes.ts), so `textToBytes` gives zmodem.js the exact bytes and
   `bytesToText` carries its reply back to the session. */

/** What the transfer needs from the disk and the user. */
export interface TransferFiles {
  /** Files to send (a multi-select dialog); undefined when cancelled. */
  pickSend(): Promise<string[] | undefined>;
  /** Where to save `name`; undefined when cancelled. */
  pickSave(name: string): Promise<string | undefined>;
  openRead(path: string): Promise<{ id: number; name: string; size: number; modified: number }>;
  read(id: number, len: number): Promise<Uint8Array>;
  /** Without `path`, the file goes to the Downloads folder under a free name. */
  openWrite(name: string, path?: string): Promise<{ id: number; path: string }>;
  write(id: number, bytes: Uint8Array): Promise<void>;
  /** A received file is kept (renamed to its final name) or removed. */
  close(id: number, keep: boolean): Promise<void>;
}

/** Where the transfer reads and writes the session. */
export interface TransferHost {
  /** Text for the terminal. */
  write(text: string): void;
  /** Text for the session (escaped bytes, see terminalBytes.ts). */
  send(text: string): Promise<void> | void;
}

export type TransferPhase = "starting" | "running" | "done" | "failed" | "cancelled";

export interface TransferView {
  /** True when the terminal's side sends (the remote ran `rz`). */
  upload: boolean;
  phase: TransferPhase;
  name: string;
  done: number;
  total: number | null;
  /** The file being handled, and how many there are when known. */
  index: number;
  count: number | null;
  startedAt: number;
  /** Where the last received file went. */
  path?: string;
  message?: string;
}

const NEGOTIATION_MS = 20_000;
const SEND_CHUNK = 32 * 1024;
const PROGRESS_MS = 100;
const RESULT_MS = 5_000;
const GIVE_UP_AFTER_CHUNKS = 3;
/** A ZMODEM hex header is 20 characters: ZPAD ZPAD ZDLE B, 14 hex digits, CR, LF. zmodem.js
 *  cannot take one in pieces, so a handshake that ends a read early is held until the rest arrives. */
const HEADER_LENGTH = 20;
const HOLD_MS = 150;

class Cancelled extends Error {}

/** Sends in the background, one write in flight at a time; `drain` waits for it to catch up. */
class Outbox {
  private pending: string[] = [];
  private busy: Promise<void> | null = null;
  private idle: (() => void)[] = [];
  error: unknown = null;

  constructor(private readonly send: (text: string) => Promise<void> | void) {}

  push(text: string): void {
    this.pending.push(text);
    if (!this.busy) this.pump();
  }

  private pump(): void {
    const text = this.pending.join("");
    this.pending = [];
    this.busy = Promise.resolve()
      .then(() => this.send(text))
      .catch((error) => {
        this.error = error;
      })
      .finally(() => {
        this.busy = null;
        if (this.pending.length > 0) this.pump();
        else for (const resolve of this.idle.splice(0)) resolve();
      });
  }

  drain(): Promise<void> {
    if (!this.busy && this.pending.length === 0) return Promise.resolve();
    return new Promise((resolve) => this.idle.push(resolve));
  }
}

/** Writes a received file in the background, merging chunks that arrive while a write is running. */
class Sink {
  private pending: Uint8Array[] = [];
  private busy: Promise<void> | null = null;
  private waiting: (() => void)[] = [];
  error: unknown = null;

  constructor(
    private readonly files: TransferFiles,
    readonly id: number,
  ) {}

  push(chunk: Uint8Array): void {
    this.pending.push(chunk);
    if (!this.busy) this.pump();
  }

  private pump(): void {
    const size = this.pending.reduce((sum, chunk) => sum + chunk.length, 0);
    const bytes = new Uint8Array(size);
    let at = 0;
    for (const chunk of this.pending) {
      bytes.set(chunk, at);
      at += chunk.length;
    }
    this.pending = [];
    this.busy = this.files
      .write(this.id, bytes)
      .catch((error) => {
        this.error = error;
      })
      .finally(() => {
        this.busy = null;
        if (this.pending.length > 0 && !this.error) this.pump();
        else for (const resolve of this.waiting.splice(0)) resolve();
      });
  }

  /** Resolves when everything pushed is on disk; throws the first write error. */
  async flush(): Promise<void> {
    if (this.busy || (this.pending.length > 0 && !this.error)) {
      await new Promise<void>((resolve) => this.waiting.push(resolve));
    }
    if (this.error) throw this.error;
  }
}

const messageOf = (error: unknown) => (error instanceof Error ? error.message : String(error));

export class TerminalTransfer {
  private host: TransferHost | null = null;
  private tail = "";
  private sentry: Sentry | null = null;
  private session: ZSession | null = null;
  private outbox: Outbox | null = null;
  private decoder = new TextDecoder();
  private misses = 0;
  private stopped = false;
  private failure: string | null = null;
  private received = 0;
  private fileNo = 0;
  private closing: (() => Promise<void>) | null = null;
  private negotiation: ReturnType<typeof setTimeout> | undefined;
  private hideTimer: ReturnType<typeof setTimeout> | undefined;
  private lastProgress = 0;
  private held = "";
  private holdTimer: ReturnType<typeof setTimeout> | undefined;
  private cancelWaiters = new Set<(error: Cancelled) => void>();
  private view: TransferView | null = null;
  private listeners = new Set<() => void>();

  constructor(
    private readonly files: TransferFiles | null,
    private readonly prefs: () => TransferPrefs,
  ) {}

  /** Connects the transfer to a terminal and its session; call before `feed`. */
  bind(host: TransferHost | null): void {
    this.host = host;
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  snapshot = (): TransferView | null => this.view;

  /** True while a ZMODEM session owns the terminal. */
  active(): boolean {
    return this.session !== null;
  }

  /** Output from the session: to the terminal, or into a transfer. */
  feed(text: string): void {
    const host = this.host;
    if (!host) return;
    if (this.sentry) {
      this.consume(textToBytes(text));
      return;
    }
    if (!this.files || !this.prefs().enabled) {
      host.write(displayText(text));
      return;
    }
    if (this.held) {
      text = this.held + text;
      this.held = "";
      this.tail = "";
      clearTimeout(this.holdTimer);
    }
    const joined = this.tail + text;
    const start = ZMODEM_START.exec(joined);
    if (!start) {
      this.tail = joined.slice(-ZMODEM_LOOKBEHIND);
      host.write(displayText(text));
      return;
    }
    // What came before the handshake is ordinary output; a few characters of
    // the handshake itself may already have been shown.
    const cut = start.index - this.tail.length;
    if (cut > 0) host.write(displayText(text.slice(0, cut)));
    this.tail = "";
    const rest = joined.slice(start.index);
    if (rest.length < HEADER_LENGTH) {
      // The header goes on in the next read; if nothing follows it was only text.
      this.held = rest;
      this.holdTimer = setTimeout(() => {
        const held = this.held;
        this.held = "";
        if (held) this.host?.write(displayText(held));
      }, HOLD_MS);
      return;
    }
    this.engage();
    this.consume(textToBytes(rest));
  }

  /** Keys typed while a transfer runs: Esc and Ctrl+C cancel it, the rest is dropped. */
  input(data: string): boolean {
    if (!this.session) return false;
    if (data === "\x1b" || data === "\x03") this.cancel();
    return true;
  }

  /** Stops the transfer the user is watching: the abort sequence goes to the remote side. */
  cancel(): void {
    if (!this.session || this.stopped) return;
    this.stopped = true;
    for (const resolve of this.cancelWaiters) resolve(new Cancelled());
    this.cancelWaiters.clear();
    try {
      this.session.abort();
    } catch {
      // Already aborted or finished.
    }
    void this.closeCurrent();
    this.ended();
  }

  /** The session or the view is going away. */
  dispose(): void {
    this.cancel();
    this.host = null;
    clearTimeout(this.negotiation);
    clearTimeout(this.hideTimer);
    clearTimeout(this.holdTimer);
    this.listeners.clear();
  }

  /** Hides a finished transfer's result. */
  dismiss(): void {
    if (this.session) return;
    this.set(null);
  }

  // ---- the stream -------------------------------------------------------

  private engage(): void {
    this.stopped = false;
    this.failure = null;
    this.received = 0;
    this.fileNo = 0;
    this.misses = 0;
    this.decoder = new TextDecoder();
    this.outbox = new Outbox((text) => this.host?.send(text));
    this.sentry = new Zmodem.Sentry({
      // While a session runs the terminal sees nothing: the protocol's own
      // bytes are not for the screen.
      to_terminal: (octets) => {
        if (this.session) return;
        this.host?.write(displayText(this.decoder.decode(Uint8Array.from(octets), { stream: true })));
      },
      sender: (octets) => this.outbox?.push(bytesToText(octets)),
      on_detect: (detection) => this.detected(detection),
      on_retract: () => {},
    });
  }

  private disengage(): void {
    this.sentry = null;
    this.tail = "";
  }

  private consume(bytes: Uint8Array): void {
    try {
      this.sentry?.consume(bytes);
    } catch (error) {
      this.fail(messageOf(error));
      return;
    }
    // A handshake that never turned into a session: stop looking.
    if (this.sentry && !this.session && (this.misses += 1) >= GIVE_UP_AFTER_CHUNKS) this.disengage();
  }

  private detected(detection: Detection): void {
    if (this.session) return;
    const upload = detection.get_session_role() === "send";
    const session = detection.confirm();
    this.session = session;
    this.stopped = false;
    this.set({ upload, phase: "starting", name: "", done: 0, total: null, index: 0, count: null, startedAt: Date.now() });
    clearTimeout(this.negotiation);
    this.negotiation = setTimeout(() => this.fail(t.transferNoAnswer), NEGOTIATION_MS);
    session.on("session_end", () => this.ended());
    if (upload) {
      void this.send(session);
    } else {
      session.on("offer", (offer) => void this.receive(offer));
      session.start().catch((error) => this.fail(messageOf(error)));
    }
  }

  // ---- state ------------------------------------------------------------

  private set(view: TransferView | null): void {
    this.view = view;
    for (const listener of this.listeners) listener();
  }

  private patch(patch: Partial<TransferView>, throttle = false): void {
    if (!this.view) return;
    const now = Date.now();
    if (throttle && now - this.lastProgress < PROGRESS_MS) {
      // Keep the numbers current for the next render without a notification.
      this.view = { ...this.view, ...patch };
      return;
    }
    this.lastProgress = now;
    this.set({ ...this.view, ...patch });
  }

  private fail(message: string): void {
    if (this.failure === null) this.failure = message;
    if (this.session && !this.stopped) {
      this.stopped = true;
      for (const resolve of this.cancelWaiters) resolve(new Cancelled());
      this.cancelWaiters.clear();
      try {
        this.session.abort();
      } catch {
        // Already aborted.
      }
      void this.closeCurrent();
    }
    this.ended();
  }

  /** The session is over, whichever way. */
  private ended(): void {
    clearTimeout(this.negotiation);
    const had = this.session !== null || this.sentry !== null;
    this.session = null;
    this.disengage();
    if (!had) return;
    const view = this.view;
    if (view) {
      const phase: TransferPhase = this.failure !== null ? "failed" : this.stopped ? "cancelled" : this.received > 0 || view.upload ? "done" : "cancelled";
      // An upload the user turned down, or a download with nothing offered, leaves no result.
      const quiet = this.failure === null && !this.stopped && view.phase === "starting";
      this.set(quiet ? null : { ...view, phase, message: this.failure ?? undefined, done: phase === "done" && view.total !== null ? view.total : view.done });
      if (this.view) {
        clearTimeout(this.hideTimer);
        if (phase !== "failed") this.hideTimer = setTimeout(() => this.dismiss(), RESULT_MS);
      }
    }
  }

  private async closeCurrent(): Promise<void> {
    const closing = this.closing;
    this.closing = null;
    if (closing) await closing().catch(() => {});
  }

  /** Waits for `work`, or throws when the transfer is cancelled first. */
  private race<T>(work: Promise<T>): Promise<T> {
    if (this.stopped) return Promise.reject(new Cancelled());
    return new Promise<T>((resolve, reject) => {
      this.cancelWaiters.add(reject);
      work.then(resolve, reject).finally(() => this.cancelWaiters.delete(reject));
    });
  }

  // ---- receiving (the remote ran sz) ---------------------------------------

  private async receive(offer: Offer): Promise<void> {
    const files = this.files;
    if (!files || this.stopped) return;
    clearTimeout(this.negotiation);
    const details = offer.get_details();
    const name = details.name;
    const size = details.size ?? null;
    this.fileNo += 1;
    const count = details.files_remaining ? this.fileNo - 1 + details.files_remaining : null;
    let sink: Sink | null = null;
    try {
      let path: string | undefined;
      if (this.prefs().askWhere) {
        path = await this.race(files.pickSave(name));
        if (path === undefined) {
          offer.skip();
          return;
        }
      }
      const opened = await this.race(files.openWrite(name, path));
      const current = new Sink(files, opened.id);
      sink = current;
      this.closing = () => files.close(opened.id, false);
      this.patch({ phase: "running", name, total: size, done: 0, index: this.fileNo, count, startedAt: Date.now(), path: opened.path });
      await this.race(
        offer.accept({
          on_input: (payload) => {
            current.push(Uint8Array.from(payload));
            this.patch({ done: offer.get_offset() }, true);
          },
        }),
      );
      await this.race(current.flush());
      this.closing = null;
      await files.close(opened.id, true);
      this.received += 1;
      this.patch({ done: size ?? offer.get_offset(), path: opened.path });
    } catch (error) {
      if (!(error instanceof Cancelled)) this.fail(messageOf(error));
      if (sink) await files.close(sink.id, false).catch(() => {});
      this.closing = null;
    }
  }

  // ---- sending (the remote ran rz) ----------------------------------------

  private async send(session: ZSession): Promise<void> {
    const files = this.files;
    if (!files) return;
    let openId: number | null = null;
    try {
      clearTimeout(this.negotiation);
      const paths = await this.race(files.pickSend());
      if (!paths || paths.length === 0) {
        // Nothing chosen: end the session politely so `rz` just returns.
        if (!this.stopped) await session.close().catch(() => session.abort());
        return;
      }
      for (let index = 0; index < paths.length; index += 1) {
        const source = await this.race(files.openRead(paths[index]));
        openId = source.id;
        this.closing = () => files.close(source.id, false);
        this.patch({ phase: "running", name: source.name, total: source.size, done: 0, index: index + 1, count: paths.length, startedAt: Date.now() });
        const transfer = await this.race(
          session.send_offer({
            name: source.name,
            size: source.size,
            mtime: new Date(source.modified * 1000),
            files_remaining: paths.length - index,
          }),
        );
        if (transfer) {
          let sent = 0;
          for (;;) {
            const chunk = await this.race(files.read(source.id, SEND_CHUNK));
            sent += chunk.length;
            if (chunk.length < SEND_CHUNK) {
              await this.race(transfer.end(chunk));
              break;
            }
            transfer.send(chunk);
            // Let the session catch up, so progress follows what really left.
            await this.race(this.outbox?.drain() ?? Promise.resolve());
            if (this.outbox?.error) throw this.outbox.error;
            this.patch({ done: sent }, true);
          }
          this.patch({ done: sent });
        }
        await files.close(source.id, false);
        openId = null;
        this.closing = null;
      }
      await this.race(session.close());
    } catch (error) {
      if (!(error instanceof Cancelled)) this.fail(messageOf(error));
      if (openId !== null) await files.close(openId, false).catch(() => {});
      this.closing = null;
    }
  }
}
