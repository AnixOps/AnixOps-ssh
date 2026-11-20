/** Batches terminal output so a flood of small events (`yes`, `cat` of a big
 *  file) costs one write per frame, and holds output back while the parser is
 *  behind, so the UI keeps getting frames.
 *
 *  The session channel has no acknowledgement, so the source cannot be
 *  paused; the queue lives here instead. The watermarks decide when to stop
 *  handing xterm more: while more than `high` characters are written but not
 *  yet parsed, nothing new is written until the count falls under `low`. */
export interface PumpSink {
  write(data: string, done: () => void): void;
}

export interface PumpOptions {
  /** Calls `run` soon; returns a cancel function. */
  schedule?: (run: () => void) => () => void;
  /** Most characters handed over in one frame. */
  frameBudget?: number;
  /** Stop writing while this many written characters are unparsed. */
  high?: number;
  /** Resume writing once the unparsed count is below this. */
  low?: number;
}

export const PUMP_FRAME_BUDGET = 512 * 1024;
export const PUMP_HIGH = 2 * 1024 * 1024;
export const PUMP_LOW = 512 * 1024;

/** The next animation frame, or 50 ms from now if the page is hidden and frames do not come. */
export function frameSchedule(run: () => void): () => void {
  let done = false;
  const fire = () => {
    if (done) return;
    done = true;
    window.cancelAnimationFrame(frame);
    window.clearTimeout(timer);
    run();
  };
  const frame = window.requestAnimationFrame(fire);
  const timer = window.setTimeout(fire, 50);
  return () => {
    done = true;
    window.cancelAnimationFrame(frame);
    window.clearTimeout(timer);
  };
}

export class OutputPump {
  private queue: string[] = [];
  private queued = 0;
  private inflight = 0;
  private waiting = false;
  private cancel: (() => void) | null = null;
  private disposed = false;
  private readonly schedule: (run: () => void) => () => void;
  private readonly budget: number;
  private readonly high: number;
  private readonly low: number;

  constructor(
    private readonly sink: PumpSink,
    options: PumpOptions = {},
  ) {
    this.schedule = options.schedule ?? frameSchedule;
    this.budget = options.frameBudget ?? PUMP_FRAME_BUDGET;
    this.high = options.high ?? PUMP_HIGH;
    this.low = options.low ?? PUMP_LOW;
  }

  /** Characters received but not yet parsed. */
  get pending(): number {
    return this.queued + this.inflight;
  }

  /** True while output is being held back for the parser. */
  get paused(): boolean {
    return this.waiting;
  }

  push(data: string): void {
    if (this.disposed || data.length === 0) return;
    this.queue.push(data);
    this.queued += data.length;
    this.arm();
  }

  dispose(): void {
    this.disposed = true;
    this.cancel?.();
    this.cancel = null;
    this.queue = [];
    this.queued = 0;
  }

  private arm(): void {
    if (this.cancel || this.disposed || this.queued === 0) return;
    this.cancel = this.schedule(() => {
      this.cancel = null;
      this.flush();
    });
  }

  private flush(): void {
    if (this.disposed) return;
    if (this.inflight >= this.high) this.waiting = true;
    if (this.waiting) {
      // Held back; a parse completion re-arms the pump once under `low`.
      if (this.inflight >= this.low) return;
      this.waiting = false;
    }
    let chunk = "";
    while (this.queue.length > 0 && chunk.length < this.budget) {
      const head = this.queue[0];
      const room = this.budget - chunk.length;
      if (head.length <= room) {
        chunk += head;
        this.queue.shift();
      } else {
        // Do not split a surrogate pair across two writes.
        let cut = room;
        const code = head.charCodeAt(cut - 1);
        if (code >= 0xd800 && code <= 0xdbff) cut -= 1;
        if (cut === 0) {
          if (chunk.length > 0) break;
          cut = room;
        }
        chunk += head.slice(0, cut);
        this.queue[0] = head.slice(cut);
      }
    }
    if (chunk.length === 0) return;
    this.queued -= chunk.length;
    this.inflight += chunk.length;
    const size = chunk.length;
    this.sink.write(chunk, () => {
      this.inflight -= size;
      this.arm();
    });
    this.arm();
  }
}
