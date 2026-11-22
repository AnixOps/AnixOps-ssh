import type { HostEnvironment } from "@/lib/backend";
import { Maximize2, Minimize2, X } from "lucide-react";
import { useRef, type CSSProperties, type KeyboardEvent, type PointerEvent } from "react";
import { TerminalView, type SessionStatus } from "@/components/app/TerminalView";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { scopeOf, type BroadcastState } from "@/lib/broadcast";
import { t } from "@/lib/i18n";
import {
  clampRatio,
  leaves,
  visibleLayout,
  type Divider,
  type PaneState,
  type Rect,
} from "@/lib/panes";
import { shortcutHint } from "@/lib/shortcutTable";
import { cn } from "@/lib/utils";

const KEY_STEP = 0.02;
const KEY_STEP_LARGE = 0.1;

const percent = (rect: Rect): CSSProperties => ({
  left: `${rect.x * 100}%`,
  top: `${rect.y * 100}%`,
  width: `${rect.w * 100}%`,
  height: `${rect.h * 100}%`,
});

/** The panes of one tab. Every pane stays mounted at a stable place in the
 *  tree, so splitting or closing never restarts the other sessions; the
 *  geometry alone moves them. */
export function PaneArea({
  tabKey,
  hostId,
  environment = null,
  hostName = "",
  state,
  restored,
  active,
  broadcast,
  onFocus,
  onResize,
  onClosePane,
  onToggleMaximise,
  onStatus,
  onSession,
}: {
  tabKey: string;
  hostId: string | null;
  /** The host's environment, for the production marks and confirmations. */
  environment?: HostEnvironment | null;
  hostName?: string;
  state: PaneState;
  /** Daemon sessions to reattach, by pane (from a restored layout). */
  restored: Record<string, string>;
  active: boolean;
  broadcast: BroadcastState;
  onFocus: (paneId: string) => void;
  onResize: (splitId: string, ratio: number) => void;
  onClosePane: (paneId: string) => void;
  onToggleMaximise: () => void;
  onStatus: (paneId: string, status: SessionStatus) => void;
  onSession: (paneId: string, sessionId: string) => void;
}) {
  const areaRef = useRef<HTMLDivElement>(null);
  const all = leaves(state.tree);
  const shown = visibleLayout(state);
  const many = all.length > 1;
  const scope = scopeOf(broadcast, tabKey);

  return (
    <div ref={areaRef} className="relative h-full min-w-0 flex-1 overflow-hidden bg-bg">
      {all.map((id) => {
        const rect = shown.panes[id];
        const focused = id === state.focused;
        return (
          <div
            key={id}
            hidden={!rect}
            style={rect ? percent(rect) : undefined}
            className="group/pane absolute"
            onPointerDownCapture={() => onFocus(id)}
            onFocusCapture={() => onFocus(id)}
          >
            <TerminalView
              tabKey={id}
              hostId={hostId}
              environment={environment}
              hostName={hostName}
              sessionId={restored[id]}
              active={active && focused && Boolean(rect)}
              broadcast={scope}
              split={Object.keys(shown.panes).length > 1 ? (focused ? "active" : "inactive") : undefined}
              onStatus={onStatus}
              onSession={onSession}
            />
            {many && (
              <div
                className={cn(
                  "absolute top-1 right-1 z-sticky flex gap-0.5 transition-opacity duration-micro",
                  state.maximised === id ? "opacity-100" : "opacity-0 group-hover/pane:opacity-100 focus-within:opacity-100",
                )}
              >
                <PaneButton
                  label={state.maximised === id ? t.restorePane : t.maximisePane}
                  hint={shortcutHint("toggle-maximise")}
                  onClick={onToggleMaximise}
                >
                  {state.maximised === id ? <Minimize2 /> : <Maximize2 />}
                </PaneButton>
                <PaneButton label={t.closePane} hint={shortcutHint("close-tab")} onClick={() => onClosePane(id)}>
                  <X />
                </PaneButton>
              </div>
            )}
          </div>
        );
      })}
      {shown.dividers.map((divider) => (
        <DividerHandle key={divider.id} divider={divider} areaRef={areaRef} onResize={onResize} />
      ))}
    </div>
  );
}

function PaneButton({
  label,
  hint,
  onClick,
  children,
}: {
  label: string;
  hint: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <Tooltip label={`${label} (${hint})`}>
      <Button
        variant="quiet"
        size="icon-sm"
        aria-label={label}
        onClick={onClick}
        className="bg-bg-elevated shadow-1 ring-1 ring-separator"
      >
        {children}
      </Button>
    </Tooltip>
  );
}

function DividerHandle({
  divider,
  areaRef,
  onResize,
}: {
  divider: Divider;
  areaRef: React.RefObject<HTMLDivElement | null>;
  onResize: (splitId: string, ratio: number) => void;
}) {
  const { axis, area, ratio, id } = divider;
  const vertical = axis === "row"; // a vertical bar splits side by side panes
  const dragging = useRef(false);

  const place: CSSProperties = vertical
    ? { left: `${(area.x + area.w * ratio) * 100}%`, top: `${area.y * 100}%`, height: `${area.h * 100}%` }
    : { top: `${(area.y + area.h * ratio) * 100}%`, left: `${area.x * 100}%`, width: `${area.w * 100}%` };

  const move = (event: PointerEvent) => {
    const box = areaRef.current?.getBoundingClientRect();
    if (!dragging.current || !box || box.width === 0 || box.height === 0) return;
    const next = vertical
      ? (event.clientX - box.left - area.x * box.width) / (area.w * box.width)
      : (event.clientY - box.top - area.y * box.height) / (area.h * box.height);
    onResize(id, clampRatio(next));
  };

  const key = (event: KeyboardEvent) => {
    const back = vertical ? "ArrowLeft" : "ArrowUp";
    const forward = vertical ? "ArrowRight" : "ArrowDown";
    const step = event.shiftKey ? KEY_STEP_LARGE : KEY_STEP;
    if (event.key === back) onResize(id, clampRatio(ratio - step));
    else if (event.key === forward) onResize(id, clampRatio(ratio + step));
    else if (event.key === "Home") onResize(id, 0);
    else if (event.key === "End") onResize(id, 1);
    else if (event.key === "Enter") onResize(id, 0.5);
    else return;
    event.preventDefault();
  };

  return (
    <div className="absolute z-dropdown" style={{ ...place, ...(vertical ? { width: 0 } : { height: 0 }) }}>
      <div
        role="separator"
        tabIndex={0}
        aria-label={t.resizePane}
        aria-orientation={vertical ? "vertical" : "horizontal"}
        aria-valuenow={Math.round(ratio * 100)}
        aria-valuemin={10}
        aria-valuemax={90}
        onPointerDown={(event) => {
          dragging.current = true;
          event.currentTarget.setPointerCapture(event.pointerId);
          event.preventDefault();
        }}
        onPointerMove={move}
        onPointerUp={(event) => {
          dragging.current = false;
          event.currentTarget.releasePointerCapture(event.pointerId);
        }}
        onDoubleClick={() => onResize(id, 0.5)}
        onKeyDown={key}
        className={cn(
          "group/divider absolute flex items-center justify-center outline-none",
          vertical ? "inset-y-0 -left-1.5 w-3 cursor-col-resize" : "inset-x-0 -top-1.5 h-3 cursor-row-resize",
        )}
      >
        <span
          className={cn(
            "bg-separator transition-colors duration-micro group-hover/divider:bg-(--accent) group-focus-visible/divider:bg-(--accent)",
            vertical ? "h-full w-px group-hover/divider:w-0.5 group-focus-visible/divider:w-0.5" : "h-px w-full group-hover/divider:h-0.5 group-focus-visible/divider:h-0.5",
          )}
        />
      </div>
    </div>
  );
}
