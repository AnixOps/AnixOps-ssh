/** Reading text out of the terminal's buffer for the assistant. Written
 *  against a small interface so it can be tested without xterm. */

export interface BufferLineLike {
  readonly isWrapped: boolean;
  translateToString(trimRight?: boolean): string;
}

export interface BufferLike {
  readonly length: number;
  getLine(y: number): BufferLineLike | undefined;
}

/** Text of lines `from` to `to` (inclusive), wrapped rows joined into one line. */
export function readLines(buffer: BufferLike, from: number, to: number): string {
  const out: string[] = [];
  for (let y = Math.max(0, from); y <= Math.min(to, buffer.length - 1); y++) {
    const line = buffer.getLine(y);
    if (!line) continue;
    const text = line.translateToString(true);
    if (line.isWrapped && out.length > 0) out[out.length - 1] += text;
    else out.push(text);
  }
  return out.join("\n");
}

/** The last `count` lines up to `bottom` (the cursor row), without blank lines at the end. */
export function lastLines(buffer: BufferLike, bottom: number, count: number): string {
  let end = Math.min(bottom, buffer.length - 1);
  while (end >= 0 && (buffer.getLine(end)?.translateToString(true) ?? "") === "") end--;
  if (end < 0) return "";
  // Count logical lines (a wrapped row belongs to the line above it).
  let start = end;
  let logical = 0;
  for (let y = end; y >= 0; y--) {
    start = y;
    if (!buffer.getLine(y)?.isWrapped) {
      logical++;
      if (logical === count) break;
    }
  }
  return readLines(buffer, start, end);
}

/** The output of a command: the lines after its prompt up to the next prompt (or `bottom`). */
export function commandOutput(
  buffer: BufferLike,
  promptLine: number,
  nextPromptLine: number | null,
  bottom: number,
): string {
  const end = nextPromptLine !== null ? nextPromptLine - 1 : bottom;
  return readLines(buffer, promptLine + 1, end).replace(/\n+$/, "");
}
