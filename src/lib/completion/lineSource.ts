/**
 * Where the text of the line being edited comes from. The keystroke tracker
 * below is the first source; shell integration can later provide another one
 * (with the cwd) behind the same interface.
 */
export interface LineSource {
  /** Tells the source what the user sent to the PTY. */
  feed(data: string): void;
  /** The current input line, or null when it is not known to be right. */
  line(): string | null;
  /** The shell reported where it is, so relative paths can be listed. */
  cwd?(): string | undefined;
  setAlternateScreen(on: boolean): void;
  /** Called after terminal output was parsed, with where the cursor now is. */
  verify(cursor: CursorPosition): void;
  /** Called with each line that the user ran. */
  onCommit(listener: (line: string) => void): void;
  reset(): void;
}

export interface CursorPosition {
  x: number;
  /** Absolute row in the buffer, so scrolling does not move it. */
  y: number;
  cols: number;
}

/** Terminal cells that `text` takes up (wide East Asian characters take two). */
export function displayWidth(text: string): number {
  let width = 0;
  for (const char of text) {
    const code = char.codePointAt(0) as number;
    if (code === 0 || (code >= 0x300 && code <= 0x36f) || code === 0x200b || (code >= 0xfe00 && code <= 0xfe0f)) continue;
    const wide =
      (code >= 0x1100 && code <= 0x115f) ||
      (code >= 0x2e80 && code <= 0xa4cf) ||
      (code >= 0xac00 && code <= 0xd7a3) ||
      (code >= 0xf900 && code <= 0xfaff) ||
      (code >= 0xfe30 && code <= 0xfe6f) ||
      (code >= 0xff00 && code <= 0xff60) ||
      (code >= 0xffe0 && code <= 0xffe6) ||
      (code >= 0x1f300 && code <= 0x1faff) ||
      (code >= 0x20000 && code <= 0x3fffd);
    width += wide ? 2 : 1;
  }
  return width;
}

/** Where the cursor is after `text` was typed starting at `anchor`. */
export function expectedCursor(anchor: CursorPosition, text: string): CursorPosition {
  const total = anchor.x + displayWidth(text);
  return { x: total % anchor.cols, y: anchor.y + Math.floor(total / anchor.cols), cols: anchor.cols };
}

const PASTE_START = "\x1b[200~";
const PASTE_END = "\x1b[201~";
const MAX_PENDING_STATES = 64;

/**
 * Rebuilds the line from the keystrokes sent to the PTY. It gives up (line()
 * is null) on anything it cannot follow, and starts again at the next Enter or
 * Ctrl+C. It also checks the terminal cursor after output, so echo-less input
 * such as passwords never counts as a line.
 */
export class KeystrokeLineSource implements LineSource {
  private buffer = "";
  private valid = true;
  private alternate = false;
  private anchor: CursorPosition | null = null;
  /** Buffers typed since the cursor last matched, newest last (echo can lag behind). */
  private pending: string[] = [];
  private verified: string | null = "";
  private listeners: ((line: string) => void)[] = [];

  /** `probe` reads the terminal cursor; without one the line is trusted as typed. */
  constructor(private readonly probe: (() => CursorPosition | null) | null = null) {}

  onCommit(listener: (line: string) => void): void {
    this.listeners.push(listener);
  }

  setAlternateScreen(on: boolean): void {
    if (this.alternate === on) return;
    this.alternate = on;
    // Back from a full screen program the shell is at an empty prompt.
    this.reset();
  }

  reset(): void {
    this.buffer = "";
    this.valid = true;
    this.anchor = null;
    this.pending = [];
    this.verified = "";
  }

  line(): string | null {
    if (this.alternate || !this.valid) return null;
    if (this.probe && this.anchor && this.verified !== this.buffer) return null;
    return this.buffer;
  }

  private invalidate() {
    this.valid = false;
    this.verified = null;
    this.pending = [];
  }

  private edited() {
    if (!this.valid) return;
    this.pending.push(this.buffer);
    if (this.pending.length > MAX_PENDING_STATES) this.pending.shift();
  }

  private append(text: string) {
    if (text === "") return;
    if (this.buffer === "" && !this.anchor && this.probe) {
      this.anchor = this.probe();
      this.pending = [""];
      this.verified = this.anchor ? "" : null;
    }
    this.buffer += text;
    this.edited();
  }

  private commit() {
    const line = this.buffer;
    const trusted = this.valid && (!this.probe || !this.anchor || this.verified === line);
    this.reset();
    if (trusted && line.trim() !== "") for (const listener of this.listeners) listener(line);
  }

  feed(data: string): void {
    if (this.alternate) return;
    let i = 0;
    while (i < data.length) {
      const char = data[i];
      if (data.startsWith(PASTE_START, i)) {
        const end = data.indexOf(PASTE_END, i + PASTE_START.length);
        const pasted = data.slice(i + PASTE_START.length, end < 0 ? data.length : end);
        if (/[\r\n\x00-\x08\x0b-\x1f\x7f]/.test(pasted)) this.invalidate();
        else this.append(pasted);
        i = end < 0 ? data.length : end + PASTE_END.length;
        continue;
      }
      if (char === "\x1b") {
        // Arrows, Home/End, Alt+key: the shell moves or rewrites the line.
        this.invalidate();
        const match = /^\x1b(?:\[[0-9;?]*[ -/]*[@-~]|O[@-~]|.)/s.exec(data.slice(i));
        i += match ? match[0].length : 1;
        continue;
      }
      i += 1;
      switch (char) {
        case "\r":
        case "\n":
          this.commit();
          break;
        case "\x03": // Ctrl+C
          this.reset();
          break;
        case "\x7f":
        case "\b": {
          if (!this.valid) break;
          const chars = [...this.buffer];
          chars.pop();
          this.buffer = chars.join("");
          this.edited();
          break;
        }
        case "\x15": // Ctrl+U
          if (!this.valid) break;
          this.buffer = "";
          this.edited();
          break;
        case "\x17": // Ctrl+W
          if (!this.valid) break;
          this.buffer = this.buffer.replace(/\S*\s*$/, "");
          this.edited();
          break;
        default:
          if (char < " ") this.invalidate();
          else {
            // Take the rest of the printable run (surrogate pairs stay together).
            let end = i;
            while (end < data.length && data[end] >= " " && data[end] !== "\x7f") end += 1;
            if (this.valid) this.append(char + data.slice(i, end));
            i = end;
          }
      }
    }
  }

  verify(cursor: CursorPosition): void {
    if (this.alternate || !this.valid || !this.probe || !this.anchor) return;
    const anchor = this.anchor;
    let first = -1;
    let last = -1;
    this.pending.forEach((text, index) => {
      const expected = expectedCursor(anchor, text);
      if (expected.x !== cursor.x || expected.y !== cursor.y) return;
      if (first < 0) first = index;
      last = index;
    });
    if (first >= 0) {
      // Echo arrives in order but may lag: the cursor can match several states
      // of the buffer. Take the newest as current, and keep the ones that the
      // echo has not reached yet.
      this.verified = this.pending[last];
      this.pending = this.pending.slice(first);
      return;
    }
    this.invalidate();
  }
}
