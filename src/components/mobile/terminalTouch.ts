import type { Terminal } from "@xterm/xterm";
import { copyText, pasteText } from "@/lib/backend";
import { cellAt, comparePos, dragHandle, lastTextColumn, selectArgs, wordRange, type Cell, type Pos, type Range } from "@/lib/touchSelect";
import { GestureTracker, pinchFontSize, swipeKeys, takeLines, type GestureEvent, type Pt } from "@/lib/touchGestures";

/* Touch on the terminal: tap to focus, long-press to select a word, drag to
   extend, scroll with one finger, scroll faster with two, pinch for the font
   size. The gesture rules are in lib/touchGestures.ts and the selection rules
   in lib/touchSelect.ts; this file reads the touches, asks xterm.js for the
   grid and applies the results. */

export interface SelectionState {
  range: Range;
}

/** What the overlay shows besides a selection: Paste alone on a long press of an empty cell. */
export interface TouchOverlay {
  selection: SelectionState | null;
  /** Where a Paste-only toolbar goes (client coordinates). */
  pastePoint: Pt | null;
}

export interface TouchDeps {
  terminal: Terminal;
  /** The element that receives the touches. */
  surface: HTMLElement;
  fontSize: () => number;
  setFontSize: (size: number, persist: boolean) => void;
  /** Bytes for the session (cursor keys, page keys). */
  sendKeys: (data: string) => void;
  /** A tap: focus the terminal and show the keyboard. */
  onTap: () => void;
  onOverlay: (overlay: TouchOverlay) => void;
  onNotice: (text: string) => void;
  haptic: () => void;
  strings: { copied: string; clipboardEmpty: string };
}

export interface TouchController {
  dispose(): void;
  /** Moves a selection handle to the cell under a client point. */
  dragHandle(which: "start" | "end", client: Pt): "start" | "end";
  clear(): void;
  selectLine(): void;
  copy(): Promise<void>;
  paste(): Promise<void>;
  hasSelection(): boolean;
}

/** Fast scrolling with two fingers moves this many times as far. */
const TWO_FINGER_FACTOR = 4;

export function attachTerminalTouch(deps: TouchDeps): TouchController {
  const { terminal, surface } = deps;
  const tracker = new GestureTracker();
  let remainder = 0;
  let timer: number | null = null;
  let range: Range | null = null;
  /** The word the long press selected; dragging extends from it. */
  let anchor: Range | null = null;
  /** Where the Paste-only toolbar sits after a long press on an empty cell. */
  let pastePoint: Pt | null = null;
  let pinchStart = 0;
  let pinchSize = 0;

  const publish = () => deps.onOverlay({ selection: range ? { range } : null, pastePoint });

  const screen = () => surface.querySelector<HTMLElement>(".xterm-screen");
  const box = () => {
    const element = screen();
    return element ? element.getBoundingClientRect() : surface.getBoundingClientRect();
  };
  const viewportTop = () => terminal.buffer.active.viewportY;

  const cellsOf = (absoluteRow: number): Cell[] => {
    const line = terminal.buffer.active.getLine(absoluteRow);
    const cells: Cell[] = [];
    if (!line) return cells;
    for (let col = 0; col < terminal.cols; col++) {
      const cell = line.getCell(col);
      cells.push(cell ? { chars: cell.getChars(), width: cell.getWidth() } : { chars: "", width: 1 });
    }
    return cells;
  };

  const cellUnder = (point: Pt): Pos => {
    const rect = box();
    const cell = cellAt(point, rect, terminal.cols, terminal.rows);
    return { col: cell.col, row: viewportTop() + cell.row };
  };

  const apply = (next: Range | null) => {
    range = next;
    if (next) {
      pastePoint = null;
      const { column, row, length } = selectArgs(next, terminal.cols);
      terminal.select(column, row, length);
    } else {
      terminal.clearSelection();
    }
    publish();
  };

  const clear = () => {
    anchor = null;
    pastePoint = null;
    if (range) apply(null);
    else publish();
  };

  const scrollBy = (dy: number, fingers: 1 | 2) => {
    const rect = box();
    const lineHeight = rect.height / Math.max(1, terminal.rows);
    const taken = takeLines(remainder, dy, lineHeight);
    remainder = taken.remainder;
    if (taken.lines === 0) return;
    // Positive lines: the finger moved down, so earlier lines come into view.
    const lines = taken.lines;
    const buffer = terminal.buffer.active;
    const mouse = terminal.modes.mouseTrackingMode !== "none";
    if (buffer.type === "alternate" && !mouse) {
      deps.sendKeys(swipeKeys(lines, fingers, terminal.modes.applicationCursorKeysMode, terminal.rows));
    } else if (mouse) {
      // The program asked for mouse events: let xterm.js encode wheel events for it.
      const wheel = new WheelEvent("wheel", {
        deltaY: -lines * (fingers === 2 ? TWO_FINGER_FACTOR : 1),
        deltaMode: WheelEvent.DOM_DELTA_LINE,
        clientX: rect.left + rect.width / 2,
        clientY: rect.top + rect.height / 2,
        bubbles: true,
        cancelable: true,
      });
      (terminal.element ?? surface).dispatchEvent(wheel);
    } else {
      terminal.scrollLines(-lines * (fingers === 2 ? TWO_FINGER_FACTOR : 1));
    }
  };

  const handle = (events: GestureEvent[]) => {
    for (const event of events) {
      switch (event.type) {
        case "tap":
          clear();
          deps.onTap();
          break;
        case "longpress": {
          const cell = cellUnder(event.at);
          const word = wordRange(cellsOf(cell.row), cell.col);
          deps.haptic();
          if (word) {
            anchor = { start: { col: word[0], row: cell.row }, end: { col: word[1], row: cell.row } };
            apply(anchor);
          } else {
            anchor = null;
            apply(null);
            pastePoint = event.at;
            publish();
          }
          break;
        }
        case "select-move": {
          if (!anchor) break;
          const cell = cellUnder(event.at);
          apply({
            start: comparePos(cell, anchor.start) < 0 ? cell : anchor.start,
            end: comparePos(cell, anchor.end) > 0 ? cell : anchor.end,
          });
          break;
        }
        case "select-end":
          publish();
          break;
        case "scroll":
          scrollBy(event.dy, event.fingers);
          break;
        case "pinch":
          if (pinchStart === 0) {
            pinchStart = deps.fontSize();
            pinchSize = pinchStart;
          }
          {
            const size = pinchFontSize(pinchStart, event.scale);
            if (size !== pinchSize) {
              pinchSize = size;
              deps.setFontSize(size, false);
            }
          }
          break;
        case "pinch-end":
          if (pinchStart !== 0) deps.setFontSize(pinchSize, true);
          pinchStart = 0;
          break;
      }
    }
  };

  const points = (list: TouchList): Pt[] => Array.from(list, (touch) => ({ x: touch.clientX, y: touch.clientY }));

  const schedule = () => {
    if (timer !== null) window.clearTimeout(timer);
    timer = null;
    const at = tracker.longPressAt;
    if (at === null) return;
    timer = window.setTimeout(
      () => {
        timer = null;
        handle(tracker.tick(performance.now()));
        // A timer can fire a fraction of a millisecond early; look again.
        schedule();
      },
      Math.max(0, at - performance.now()),
    );
  };

  /* xterm.js replaces its row elements while it renders, and a touch keeps
     going to the element it started on even when that element has left the
     page, so its later events never bubble to the surface. The move, end and
     cancel listeners therefore sit on each touch's own target. */
  const tracked = new Set<EventTarget>();
  const untrack = () => {
    for (const target of tracked) {
      target.removeEventListener("touchmove", onMove as EventListener);
      target.removeEventListener("touchend", onEnd as EventListener);
      target.removeEventListener("touchcancel", onCancel as EventListener);
    }
    tracked.clear();
  };

  const onStart = (event: TouchEvent) => {
    // The selection handles and toolbar sit on the surface and take their own touches.
    const first = event.target;
    if (first instanceof Element && first.closest("button, [data-handle], [role=toolbar]")) return;
    for (const touch of Array.from(event.changedTouches)) {
      const target = touch.target;
      if (!target || tracked.has(target)) continue;
      tracked.add(target);
      target.addEventListener("touchmove", onMove as EventListener, { passive: false });
      target.addEventListener("touchend", onEnd as EventListener, { passive: false });
      target.addEventListener("touchcancel", onCancel as EventListener, { passive: true });
    }
    const all = points(event.touches);
    remainder = 0;
    handle(tracker.start(all, performance.now()));
    schedule();
  };
  const onMove = (event: TouchEvent) => {
    // The app scrolls, selects and zooms; the page must not.
    if (event.cancelable) event.preventDefault();
    // xterm.js would scroll its viewport by the touch as well, and fight this.
    event.stopPropagation();
    handle(tracker.move(points(event.touches), performance.now()));
    schedule();
  };
  const onEnd = (event: TouchEvent) => {
    // No emulated mouse events afterwards: xterm.js would clear the selection
    // on the mousedown and the tap is handled here.
    if (event.cancelable) event.preventDefault();
    handle(tracker.end(points(event.touches), performance.now()));
    schedule();
    if (event.touches.length === 0) untrack();
  };
  const onCancel = () => {
    tracker.cancel();
    schedule();
    untrack();
  };
  const onContextMenu = (event: Event) => event.preventDefault();

  surface.addEventListener("touchstart", onStart, { passive: true });
  surface.addEventListener("contextmenu", onContextMenu);

  return {
    dispose() {
      if (timer !== null) window.clearTimeout(timer);
      surface.removeEventListener("touchstart", onStart);
      untrack();
      surface.removeEventListener("contextmenu", onContextMenu);
    },
    dragHandle(which, client) {
      if (!range) return which;
      // The finger covers the handle; aim one line above it.
      const rect = box();
      const lift = rect.height / Math.max(1, terminal.rows);
      const cell = cellUnder({ x: client.x, y: client.y - lift });
      const moved = dragHandle(range, which, cell);
      apply(moved.range);
      return moved.which;
    },
    clear,
    selectLine() {
      if (!range) return;
      const row = range.start.row;
      apply({ start: { col: 0, row }, end: { col: lastTextColumn(cellsOf(row)), row } });
    },
    async copy() {
      const text = terminal.getSelection();
      if (!text) return;
      try {
        await copyText(text);
        deps.onNotice(deps.strings.copied);
        deps.haptic();
      } catch (reason) {
        deps.onNotice(String(reason));
      }
      clear();
    },
    async paste() {
      let text = "";
      try {
        text = await pasteText();
      } catch (reason) {
        deps.onNotice(String(reason));
      }
      if (!text) {
        deps.onNotice(deps.strings.clipboardEmpty);
      } else {
        // Bracketed when the shell asks for it.
        terminal.paste(text);
      }
      clear();
      deps.onTap();
    },
    hasSelection: () => range !== null,
  };
}
