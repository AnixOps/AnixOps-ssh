import type { Terminal } from "@xterm/xterm";
import { ClipboardPaste, Copy, TextSelect } from "lucide-react";
import { useRef, type PointerEvent } from "react";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import { toolbarPlacement } from "@/lib/touchSelect";
import type { TouchController, TouchOverlay } from "@/components/mobile/terminalTouch";

const HANDLE = 44;
const TOOLBAR_HEIGHT = 52;

/** The two drag handles of a selection and the Copy / Paste toolbar. Positions
 *  are computed from the terminal's grid at render time; the parent renders
 *  again on scroll, resize and selection change. Everything keeps the terminal
 *  focused (pointer-down is swallowed) so the keyboard stays open. */
export function SelectionOverlay({
  terminal,
  surface,
  wrapper,
  overlay,
  controller,
}: {
  terminal: Terminal;
  surface: HTMLElement | null;
  wrapper: HTMLElement | null;
  overlay: TouchOverlay;
  controller: TouchController | null;
}) {
  const dragging = useRef<"start" | "end" | null>(null);
  const screen = surface?.querySelector<HTMLElement>(".xterm-screen");
  if (!screen || !wrapper || !controller || (!overlay.selection && !overlay.pastePoint)) return null;

  const screenBox = screen.getBoundingClientRect();
  const wrapperBox = wrapper.getBoundingClientRect();
  const originX = screenBox.left - wrapperBox.left;
  const originY = screenBox.top - wrapperBox.top;
  const cellWidth = screenBox.width / Math.max(1, terminal.cols);
  const cellHeight = screenBox.height / Math.max(1, terminal.rows);
  const top = terminal.buffer.active.viewportY;

  const swallow = (event: PointerEvent) => event.preventDefault();

  let toolbarX = 0;
  let toolbarY = 0;
  let handles: { which: "start" | "end"; x: number; y: number }[] = [];
  if (overlay.selection) {
    const { start, end } = overlay.selection.range;
    const startRow = start.row - top;
    const endRow = end.row - top;
    const startX = originX + start.col * cellWidth;
    const endX = originX + (end.col + 1) * cellWidth;
    const visible = (row: number) => row >= 0 && row < terminal.rows;
    handles = [
      ...(visible(startRow) ? [{ which: "start" as const, x: startX, y: originY + (startRow + 1) * cellHeight }] : []),
      ...(visible(endRow) ? [{ which: "end" as const, x: endX, y: originY + (endRow + 1) * cellHeight }] : []),
    ];
    const clampedStart = Math.max(0, startRow);
    const above = toolbarPlacement(clampedStart, cellHeight, TOOLBAR_HEIGHT) === "above";
    toolbarX = startRow === endRow ? (startX + endX) / 2 : wrapperBox.width / 2;
    toolbarY = above
      ? originY + clampedStart * cellHeight - TOOLBAR_HEIGHT - 4
      : originY + (Math.min(terminal.rows - 1, Math.max(0, endRow)) + 1) * cellHeight + HANDLE;
  } else if (overlay.pastePoint) {
    toolbarX = overlay.pastePoint.x - wrapperBox.left;
    toolbarY = overlay.pastePoint.y - wrapperBox.top - TOOLBAR_HEIGHT - 24;
  }
  const half = Math.min(overlay.selection ? 176 : 60, (wrapperBox.width - 8) / 2);
  toolbarX = Math.min(Math.max(toolbarX, half + 4), Math.max(half + 4, wrapperBox.width - half - 4));
  toolbarY = Math.min(Math.max(toolbarY, 4), Math.max(4, wrapperBox.height - TOOLBAR_HEIGHT - 4));

  return (
    <div className="pointer-events-none absolute inset-0 z-10">
      {handles.map((handle) => (
        <button
          key={handle.which}
          type="button"
          data-handle={handle.which}
          aria-label={handle.which === "start" ? t.mobileSelectionStart : t.mobileSelectionEnd}
          className="touch-surface pointer-events-auto absolute flex items-start justify-center"
          style={{ left: handle.x - HANDLE / 2, top: handle.y - 4, width: HANDLE, height: HANDLE }}
          onPointerDown={(event) => {
            event.preventDefault();
            event.currentTarget.setPointerCapture(event.pointerId);
            dragging.current = handle.which;
          }}
          onPointerMove={(event) => {
            if (dragging.current) {
              dragging.current = controller.dragHandle(dragging.current, { x: event.clientX, y: event.clientY });
            }
          }}
          onPointerUp={() => (dragging.current = null)}
          onPointerCancel={() => (dragging.current = null)}
        >
          <span aria-hidden className="mt-1 size-3.5 rounded-full bg-accent-fill shadow-2 ring-2 ring-bg" />
        </button>
      ))}
      <div
        role="toolbar"
        aria-label={t.mobileSelection}
        onPointerDown={swallow}
        className="pointer-events-auto absolute flex -translate-x-1/2 items-center gap-1 rounded-pill bg-bg-elevated p-1 shadow-3 ring-1 ring-separator"
        style={{ left: toolbarX, top: toolbarY, maxWidth: wrapperBox.width - 8 }}
      >
        {overlay.selection && (
          <>
            <Button variant="quiet" size="lg" onClick={() => void controller.copy()}>
              <Copy />
              {t.copy}
            </Button>
            <Button variant="quiet" size="lg" onClick={() => controller.selectLine()}>
              <TextSelect />
              {t.mobileSelectAll}
            </Button>
          </>
        )}
        <Button variant="quiet" size="lg" onClick={() => void controller.paste()}>
          <ClipboardPaste />
          {t.mobilePasteHere}
        </Button>
      </div>
    </div>
  );
}
