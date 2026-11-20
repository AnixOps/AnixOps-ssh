/* Output back-pressure for a terminal view. Session output arrives in many
   small events; `cat` of a big file produces thousands per second and writing
   each one to xterm.js freezes the window. The buffer joins what arrives
   between two animation frames into one write, counts the characters that are
   queued or still being parsed (xterm's write callback says when a write is
   done), and reports when that count crosses a high or low mark so a backend
   that can pause reads may do so. */

/** Above this many pending characters the source should stop sending. */
export const HIGH_WATER_MARK = 2 * 1024 * 1024;
/** Below this many it may continue. */
export const LOW_WATER_MARK = 512 * 1024;

export interface OutputBufferOptions {
  /** Hands data to the terminal; `done` is called once it has been parsed. */
  write: (data: string, done: () => void) => void;
  /** Runs `flush` at the next frame; returns a function that cancels it. */
  schedule: (flush: () => void) => () => void;
  highWater?: number;
  lowWater?: number;
  /** The pending size passed the high mark. */
  onPause?: () => void;
  /** The pending size fell below the low mark after a pause. */
  onResume?: () => void;
}

export class OutputBuffer {
  private chunks: string[] = [];
  private queued = 0;
  private inFlight = 0;
  private cancel: (() => void) | null = null;
  private paused = false;
  private disposed = false;
  private readonly highWater: number;
  private readonly lowWater: number;

  constructor(private readonly options: OutputBufferOptions) {
    this.highWater = options.highWater ?? HIGH_WATER_MARK;
    this.lowWater = options.lowWater ?? LOW_WATER_MARK;
  }

  /** Characters queued or being parsed. */
  get pending(): number {
    return this.queued + this.inFlight;
  }

  get isPaused(): boolean {
    return this.paused;
  }

  push(data: string): void {
    if (this.disposed || data.length === 0) return;
    this.chunks.push(data);
    this.queued += data.length;
    this.cancel ??= this.options.schedule(() => this.flush());
    if (!this.paused && this.pending > this.highWater) {
      this.paused = true;
      this.options.onPause?.();
    }
  }

  /** Writes everything queued as one chunk. */
  flush(): void {
    this.cancel?.();
    this.cancel = null;
    if (this.disposed || this.chunks.length === 0) return;
    const data = this.chunks.length === 1 ? this.chunks[0] : this.chunks.join("");
    const size = this.queued;
    this.chunks = [];
    this.queued = 0;
    this.inFlight += size;
    this.options.write(data, () => {
      this.inFlight -= size;
      this.maybeResume();
    });
  }

  dispose(): void {
    this.disposed = true;
    this.cancel?.();
    this.cancel = null;
    this.chunks = [];
    this.queued = 0;
  }

  private maybeResume(): void {
    if (this.disposed || !this.paused || this.pending > this.lowWater) return;
    this.paused = false;
    this.options.onResume?.();
  }
}

/** The browser's frame scheduler as `OutputBuffer` wants it. */
export function animationFrameSchedule(flush: () => void): () => void {
  const frame = window.requestAnimationFrame(flush);
  return () => window.cancelAnimationFrame(frame);
}
