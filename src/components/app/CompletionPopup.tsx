import { CornerDownRight, File, Flag, Folder, History, Server, Tag, Terminal, type LucideIcon } from "lucide-react";
import { useEffect, useRef } from "react";
import { t } from "@/lib/i18n";
import type { PopupState } from "@/lib/completion/popup";
import type { CandidateKind } from "@/lib/completion/types";
import { cn } from "@/lib/utils";

const ICONS: Record<CandidateKind, LucideIcon> = {
  command: Terminal,
  subcommand: CornerDownRight,
  option: Flag,
  value: Tag,
  file: File,
  folder: Folder,
  history: History,
  host: Server,
};

export interface PopupPlacement {
  left: number;
  /** Set to put the list below the cursor line. */
  top?: number;
  /** Set to put the list above the cursor line. */
  bottom?: number;
}

export function CompletionPopup({
  popup,
  placement,
  onPick,
  onHover,
}: {
  popup: PopupState;
  placement: PopupPlacement;
  onPick: (index: number) => void;
  onHover: (index: number) => void;
}) {
  const listRef = useRef<HTMLDivElement>(null);
  const shown = popup.index ?? 0;
  const item = popup.items[shown];

  useEffect(() => {
    const element = listRef.current?.querySelector<HTMLElement>(`[data-index="${shown}"]`);
    element?.scrollIntoView({ block: "nearest" });
  }, [shown, popup.items]);

  return (
    <div
      className="material-bar absolute z-dropdown w-80 max-w-[calc(100%-1rem)] overflow-hidden rounded-sm shadow-3 ring-1 ring-separator"
      style={{ left: placement.left, top: placement.top, bottom: placement.bottom }}
    >
      <div ref={listRef} role="listbox" aria-label={t.completionList} className="max-h-64 overflow-y-auto p-1">
        {popup.items.map((entry, index) => {
          const Icon = ICONS[entry.kind];
          const selected = popup.index === index;
          return (
            <div
              key={`${entry.kind}:${entry.insert}`}
              id={`completion-${index}`}
              role="option"
              aria-selected={selected}
              data-index={index}
              // Keep keyboard focus in the terminal.
              onMouseDown={(event) => {
                event.preventDefault();
                onPick(index);
              }}
              onMouseMove={() => onHover(index)}
              className={cn("flex h-8 items-center gap-2 rounded-xs px-2", selected && "bg-accent-soft")}
            >
              <Icon className={cn("size-4 shrink-0", selected ? "text-(--accent)" : "text-label-2")} strokeWidth={1.5} />
              <span className="min-w-0 flex-1 truncate font-mono text-callout text-label-1">{entry.label}</span>
            </div>
          );
        })}
      </div>
      {item?.description && (
        <p className="border-t px-3 py-1.5 text-callout text-label-2">{item.description}</p>
      )}
    </div>
  );
}
