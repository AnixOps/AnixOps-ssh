/* Touch selection in a terminal grid, as plain functions.

   xterm.js selects with a mouse. For touch the app adds what a phone person
   expects: long-press a word to select it, then drag two handles to adjust
   the range, and a Copy / Paste toolbar. The geometry and the rules live here
   so they can be tested; the component only reads touches and calls
   `terminal.select`. Rows are absolute buffer rows, so a selection survives
   scrolling. */

export interface Pos {
  col: number;
  /** Absolute row in the buffer (viewport top + row on screen). */
  row: number;
}

/** A selection from `start` to `end`, both included, `start` first. */
export interface Range {
  start: Pos;
  end: Pos;
}

/** One grid cell as xterm reports it: its text and how many columns the
 *  character covers (2 for a wide character, whose second column has width 0). */
export interface Cell {
  chars: string;
  width: number;
}

export interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Characters that end a word. Path, URL and identifier characters
 *  (`/ . - _ : @ ~ = # % + ?`) do not, so a long-press selects a whole path or URL. */
const SEPARATORS = new Set([...` \t"'\`()[]{}<>|;, 　`]);

function isWordCell(cell: Cell | undefined): boolean {
  if (!cell) return false;
  if (cell.width === 0) return true; // second half of a wide character: same word as the first
  if (cell.chars === "") return false;
  return ![...cell.chars].some((char) => SEPARATORS.has(char));
}

/** The first column of the character that covers `col`. */
function leadColumn(cells: Cell[], col: number): number {
  let lead = col;
  while (lead > 0 && cells[lead]?.width === 0) lead--;
  return lead;
}

/** The word under `col` as `[first, last]` columns, or `null` on a blank cell. */
export function wordRange(cells: Cell[], col: number): [number, number] | null {
  if (col < 0 || col >= cells.length) return null;
  const lead = leadColumn(cells, col);
  if (!isWordCell(cells[lead])) return null;
  let first = lead;
  while (first > 0 && isWordCell(cells[first - 1])) first--;
  first = leadColumn(cells, first);
  let last = lead;
  while (last + 1 < cells.length && isWordCell(cells[last + 1])) last++;
  return [first, last];
}

/** The last used column of a line (trailing blanks do not count). */
export function lastTextColumn(cells: Cell[]): number {
  for (let col = cells.length - 1; col >= 0; col--) {
    if (cells[col].chars.trim() !== "" || cells[col].width === 0) return col;
  }
  return 0;
}

const clamp = (value: number, low: number, high: number) => Math.min(Math.max(value, low), high);

/** The cell under a point (client coordinates) of a grid drawn in `box`. */
export function cellAt(point: { x: number; y: number }, box: Box, cols: number, rows: number): { col: number; row: number } {
  const cellWidth = box.width / Math.max(1, cols);
  const cellHeight = box.height / Math.max(1, rows);
  return {
    col: clamp(Math.floor((point.x - box.left) / cellWidth), 0, cols - 1),
    row: clamp(Math.floor((point.y - box.top) / cellHeight), 0, rows - 1),
  };
}

export function comparePos(a: Pos, b: Pos): number {
  return a.row - b.row || a.col - b.col;
}

export function orderRange(a: Pos, b: Pos): Range {
  return comparePos(a, b) <= 0 ? { start: a, end: b } : { start: b, end: a };
}

/** Moves one handle to `to`. Dragging a handle past the other turns it into
 *  the other one, so the range never inverts; `which` is the handle that is
 *  now under the finger. */
export function dragHandle(range: Range, which: "start" | "end", to: Pos): { range: Range; which: "start" | "end" } {
  if (which === "start") {
    return comparePos(to, range.end) > 0
      ? { range: { start: range.end, end: to }, which: "end" }
      : { range: { start: to, end: range.end }, which: "start" };
  }
  return comparePos(to, range.start) < 0
    ? { range: { start: to, end: range.start }, which: "start" }
    : { range: { start: range.start, end: to }, which: "end" };
}

/** Arguments of xterm's `terminal.select(column, row, length)`: the length
 *  counts cells and wraps from line to line. */
export function selectArgs(range: Range, cols: number): { column: number; row: number; length: number } {
  return {
    column: range.start.col,
    row: range.start.row,
    length: (range.end.row - range.start.row) * cols + (range.end.col - range.start.col) + 1,
  };
}

/** Where the Copy / Paste toolbar goes: above the selection when there is
 *  room for it, otherwise below. */
export function toolbarPlacement(startRowOnScreen: number, cellHeight: number, toolbarHeight: number): "above" | "below" {
  return startRowOnScreen * cellHeight >= toolbarHeight + cellHeight ? "above" : "below";
}

/** A finger covers what it touches, so a dragged handle aims one line above
 *  the touch point. */
export const HANDLE_LIFT_ROWS = 1;
