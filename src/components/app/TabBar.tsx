import { Columns2, FolderOpen, LoaderCircle, Plus, Radio, RadioTower, Rows2, SquareTerminal, Unplug, X } from "lucide-react";
import { AnimatePresence, motion } from "motion/react";
import type { SessionStatus } from "@/components/app/TerminalView";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { scopeOf, toggleGlobalBroadcast, toggleTabBroadcast, useBroadcast } from "@/lib/broadcast";
import { t } from "@/lib/i18n";
import { duration, ease, reflow, slide } from "@/lib/motion";
import type { Axis, PaneState } from "@/lib/panes";
import { shortcutHint, shortcutLabel } from "@/lib/shortcuts";
import { hostColor } from "@/lib/backend";
import { cn } from "@/lib/utils";

export interface Tab {
  key: string;
  /** null for a local shell. */
  hostId: string | null;
  title: string;
  color: string | null;
  /** The focused pane's status. */
  status: SessionStatus;
  /** The pane layout; every pane is its own session. */
  panes: PaneState;
  paneStatus: Record<string, SessionStatus>;
  /** Sessions that outlived a reload, by pane: the view reattaches instead of opening one. */
  restored: Record<string, string>;
  filesOpen: boolean;
  /** Set once the pane has been opened; it then stays mounted so transfers survive closing it. */
  filesMounted: boolean;
}

export function TabBar({
  tabs,
  activeKey,
  onSelect,
  onClose,
  onNew,
  onToggleFiles,
  onSplit,
}: {
  tabs: Tab[];
  activeKey: string | null;
  onSelect: (key: string) => void;
  onClose: (key: string) => void;
  onNew: () => void;
  onToggleFiles: () => void;
  onSplit: (axis: Axis) => void;
}) {
  const activeTab = tabs.find((tab) => tab.key === activeKey);
  const broadcast = useBroadcast();
  const tabBroadcast = activeTab ? broadcast.tabs.has(activeTab.key) : false;
  return (
    <div className="material-bar flex h-11 shrink-0 items-center gap-1 border-b px-2">
      <div role="tablist" className="flex min-w-0 items-center gap-1 overflow-x-auto">
        <AnimatePresence initial={false}>
        {tabs.map((tab) => {
          const active = tab.key === activeKey;
          return (
            <motion.div
              key={tab.key}
              layout="position"
              initial={{ opacity: 0, scale: 0.92 }}
              animate={{ opacity: 1, scale: 1, transition: { duration: duration.toggle, ease: ease.emphasized } }}
              exit={{ opacity: 0, scale: 0.92, transition: { duration: duration.micro, ease: ease.exit } }}
              transition={reflow}
              className={cn(
                "group relative flex h-8 max-w-52 shrink-0 origin-left items-center rounded-sm transition-colors duration-micro ease-standard",
                active ? "text-label-1" : "text-label-2 hover:bg-fill-1",
              )}
            >
              {active && (
                <motion.span
                  layoutId="tab-active"
                  transition={slide}
                  className="absolute inset-0 rounded-sm bg-bg-elevated shadow-1 ring-1 ring-separator"
                />
              )}
              <button
                type="button"
                role="tab"
                aria-selected={active}
                onClick={() => onSelect(tab.key)}
                onAuxClick={(event) => event.button === 1 && onClose(tab.key)}
                className="relative flex h-full min-w-0 items-center gap-2 pl-3 text-callout"
              >
                <TabIcon tab={tab} />
                <span className="truncate">{tab.title}</span>
                {scopeOf(broadcast, tab.key) && (
                  <Radio className="size-3.5 shrink-0 text-warning" strokeWidth={1.5} aria-label={t.broadcastingTab} />
                )}
              </button>
              <Tooltip label={`${t.closeTab} (${shortcutLabel("w")})`}>
                <button
                  type="button"
                  aria-label={t.closeTab}
                  onClick={() => onClose(tab.key)}
                  className={cn(
                    "relative mx-1 flex size-5 items-center justify-center rounded-xs text-label-3 transition-opacity duration-micro hover:bg-fill-2 hover:text-label-1",
                    !active && "opacity-0 group-hover:opacity-100 focus-visible:opacity-100",
                  )}
                >
                  <X className="size-3.5" strokeWidth={1.5} />
                </button>
              </Tooltip>
            </motion.div>
          );
        })}
        </AnimatePresence>
      </div>
      <Tooltip label={`${t.newTab} (${shortcutLabel("t")})`}>
        <Button variant="ghost" size="icon-sm" aria-label={t.newTab} onClick={onNew}>
          <Plus />
        </Button>
      </Tooltip>
      {activeTab && (
        <div className="ml-auto flex items-center gap-0.5">
          <Tooltip label={`${t.splitRight} (${shortcutHint("split-right")})`}>
            <Button variant="ghost" size="icon-sm" aria-label={t.splitRight} onClick={() => onSplit("row")}>
              <Columns2 />
            </Button>
          </Tooltip>
          <Tooltip label={`${t.splitDown} (${shortcutHint("split-down")})`}>
            <Button variant="ghost" size="icon-sm" aria-label={t.splitDown} onClick={() => onSplit("col")}>
              <Rows2 />
            </Button>
          </Tooltip>
          <Tooltip label={`${t.broadcastTab} (${shortcutHint("toggle-broadcast")})`}>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t.broadcastTab}
              aria-pressed={tabBroadcast}
              onClick={() => toggleTabBroadcast(activeTab.key)}
              className={cn(tabBroadcast && "bg-warning-soft text-warning hover:bg-warning-soft hover:text-warning")}
            >
              <Radio />
            </Button>
          </Tooltip>
          <Tooltip label={t.broadcastAll}>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={t.broadcastAll}
              aria-pressed={broadcast.global}
              onClick={toggleGlobalBroadcast}
              className={cn(broadcast.global && "bg-warning-soft text-warning hover:bg-warning-soft hover:text-warning")}
            >
              <RadioTower />
            </Button>
          </Tooltip>
        </div>
      )}
      {activeTab?.hostId && (
        <Tooltip label={`${t.toggleFiles} (${shortcutLabel("e")})`}>
          <Button
            variant="ghost"
            size="sm"
            aria-pressed={activeTab.filesOpen}
            onClick={onToggleFiles}
            className={cn(activeTab.filesOpen && "bg-accent-soft text-(--accent) hover:bg-accent-soft hover:text-(--accent)")}
          >
            <FolderOpen />
            {t.files}
          </Button>
        </Tooltip>
      )}
    </div>
  );
}

/** Status is shown as an icon with an accessible label, never colour alone. */
function TabIcon({ tab }: { tab: Tab }) {
  const icon = "size-3.5 shrink-0";
  if (tab.status.state === "connecting") {
    return (
      <LoaderCircle
        className={cn(icon, "animate-spin text-label-3 motion-reduce:animate-none")}
        strokeWidth={1.5}
        aria-label={t.connecting}
      />
    );
  }
  if (tab.status.state === "reconnecting") {
    return (
      <LoaderCircle
        className={cn(icon, "animate-spin text-label-3 motion-reduce:animate-none")}
        strokeWidth={1.5}
        aria-label={t.reconnecting}
      />
    );
  }
  if (tab.status.state === "exited") {
    return <Unplug className={cn(icon, "text-label-3")} strokeWidth={1.5} aria-label={t.sessionEnded} />;
  }
  if (tab.hostId === null) {
    return <SquareTerminal className={cn(icon, "text-label-2")} strokeWidth={1.5} aria-label={t.connected} />;
  }
  return (
    <span
      role="img"
      aria-label={t.connected}
      className="size-2 shrink-0 rounded-full"
      style={{ backgroundColor: hostColor(tab.color) ?? "var(--accent)" }}
    />
  );
}
