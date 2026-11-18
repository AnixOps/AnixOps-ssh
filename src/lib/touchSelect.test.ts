import { describe, expect, it } from "vitest";
import {
  cellAt,
  comparePos,
  dragHandle,
  lastTextColumn,
  orderRange,
  selectArgs,
  toolbarPlacement,
  wordRange,
  type Cell,
} from "./touchSelect";

/** A line of cells from text; a wide character takes two columns. */
function line(text: string, cols = text.length): Cell[] {
  const cells: Cell[] = [];
  for (const char of text) {
    const wide = /[　-鿿＀-￯]/.test(char);
    cells.push({ chars: char, width: wide ? 2 : 1 });
    if (wide) cells.push({ chars: "", width: 0 });
  }
  while (cells.length < cols) cells.push({ chars: "", width: 1 });
  return cells;
}

describe("word under a long press", () => {
  const cells = line("ls -la /var/log/syslog  done", 40);

  it("selects the word and stops at spaces", () => {
    expect(wordRange(cells, 0)).toEqual([0, 1]);
    expect(wordRange(cells, 1)).toEqual([0, 1]);
    expect(wordRange(cells, 4)).toEqual([3, 5]);
  });

  it("keeps a whole path together", () => {
    expect(wordRange(cells, 8)).toEqual([7, 21]);
    expect(wordRange(cells, 21)).toEqual([7, 21]);
  });

  it("finds nothing on a space or an empty cell", () => {
    expect(wordRange(cells, 2)).toBeNull();
    expect(wordRange(cells, 22)).toBeNull();
    expect(wordRange(cells, 39)).toBeNull();
    expect(wordRange(cells, -1)).toBeNull();
    expect(wordRange(cells, 99)).toBeNull();
  });

  it("splits at quotes, brackets and pipes but not at dots, dashes or colons", () => {
    const text = line('echo "a.b-c:d" (x)|y', 30);
    expect(wordRange(text, 7)).toEqual([6, 12]);
    expect(wordRange(text, 16)).toEqual([16, 16]);
    expect(wordRange(text, 19)).toEqual([19, 19]);
  });

  it("selects an URL", () => {
    const url = line("see https://example.org/a?b=1#c ok", 40);
    expect(wordRange(url, 10)).toEqual([4, 30]);
  });

  it("treats a wide character's second column as the same character", () => {
    const cjk = line("你好 world", 20);
    // 你 = columns 0-1, 好 = 2-3, space = 4.
    expect(wordRange(cjk, 1)).toEqual([0, 3]);
    expect(wordRange(cjk, 3)).toEqual([0, 3]);
    expect(wordRange(cjk, 4)).toBeNull();
    expect(wordRange(cjk, 6)).toEqual([5, 9]);
  });
});

describe("last used column", () => {
  it("ignores trailing blanks", () => {
    expect(lastTextColumn(line("abc", 10))).toBe(2);
    expect(lastTextColumn(line("", 10))).toBe(0);
    expect(lastTextColumn(line("你好", 10))).toBe(3);
  });
});

describe("pixels to cells", () => {
  const box = { left: 10, top: 20, width: 400, height: 240 };

  it("maps a point to its column and row", () => {
    // 80 x 24 cells of 5 x 10 px.
    expect(cellAt({ x: 10, y: 20 }, box, 80, 24)).toEqual({ col: 0, row: 0 });
    expect(cellAt({ x: 14.9, y: 29.9 }, box, 80, 24)).toEqual({ col: 0, row: 0 });
    expect(cellAt({ x: 15, y: 30 }, box, 80, 24)).toEqual({ col: 1, row: 1 });
    expect(cellAt({ x: 209, y: 140 }, box, 80, 24)).toEqual({ col: 39, row: 12 });
  });

  it("clamps points outside the grid", () => {
    expect(cellAt({ x: -50, y: -50 }, box, 80, 24)).toEqual({ col: 0, row: 0 });
    expect(cellAt({ x: 900, y: 900 }, box, 80, 24)).toEqual({ col: 79, row: 23 });
  });
});

describe("handles", () => {
  const range = { start: { col: 3, row: 5 }, end: { col: 8, row: 5 } };

  it("orders positions by row, then column", () => {
    expect(comparePos({ col: 9, row: 1 }, { col: 0, row: 2 })).toBeLessThan(0);
    expect(comparePos({ col: 1, row: 2 }, { col: 0, row: 2 })).toBeGreaterThan(0);
    expect(orderRange({ col: 5, row: 2 }, { col: 1, row: 2 })).toEqual({
      start: { col: 1, row: 2 },
      end: { col: 5, row: 2 },
    });
  });

  it("moves the start handle backwards and forwards", () => {
    expect(dragHandle(range, "start", { col: 0, row: 4 })).toEqual({
      range: { start: { col: 0, row: 4 }, end: range.end },
      which: "start",
    });
    expect(dragHandle(range, "start", { col: 6, row: 5 }).range.start).toEqual({ col: 6, row: 5 });
  });

  it("swaps the handles when one is dragged past the other", () => {
    const moved = dragHandle(range, "start", { col: 12, row: 5 });
    expect(moved.range).toEqual({ start: { col: 8, row: 5 }, end: { col: 12, row: 5 } });
    expect(moved.which).toBe("end");
    const back = dragHandle(range, "end", { col: 1, row: 5 });
    expect(back.range).toEqual({ start: { col: 1, row: 5 }, end: { col: 3, row: 5 } });
    expect(back.which).toBe("start");
  });

  it("a range never inverts, however the handles are dragged", () => {
    let current = { range, which: "end" as "start" | "end" };
    for (const to of [{ col: 0, row: 0 }, { col: 20, row: 9 }, { col: 4, row: 5 }, { col: 4, row: 2 }]) {
      current = dragHandle(current.range, current.which, to);
      expect(comparePos(current.range.start, current.range.end)).toBeLessThanOrEqual(0);
    }
  });
});

describe("the arguments for terminal.select", () => {
  it("counts cells within a row", () => {
    expect(selectArgs({ start: { col: 3, row: 5 }, end: { col: 8, row: 5 } }, 80)).toEqual({ column: 3, row: 5, length: 6 });
    expect(selectArgs({ start: { col: 4, row: 5 }, end: { col: 4, row: 5 } }, 80)).toEqual({ column: 4, row: 5, length: 1 });
  });

  it("wraps across rows", () => {
    expect(selectArgs({ start: { col: 70, row: 5 }, end: { col: 9, row: 6 } }, 80)).toEqual({ column: 70, row: 5, length: 20 });
    expect(selectArgs({ start: { col: 0, row: 1 }, end: { col: 79, row: 3 } }, 80)).toEqual({ column: 0, row: 1, length: 240 });
  });
});

describe("toolbar placement", () => {
  it("goes above the selection unless it is at the top", () => {
    expect(toolbarPlacement(10, 20, 44)).toBe("above");
    expect(toolbarPlacement(1, 20, 44)).toBe("below");
    expect(toolbarPlacement(3, 20, 44)).toBe("below");
    expect(toolbarPlacement(4, 20, 44)).toBe("above");
  });
});
