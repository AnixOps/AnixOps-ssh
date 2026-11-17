import { Cable, Columns2, FolderOpen, LoaderCircle, Plus, Radio, RadioTower, Rows2, SquareTerminal, Unplug, X } from "lucide-react";
import type { KeyboardEvent } from "react";
import { AnimatePresence, motion } from "motion/react";
import { Menu, type MenuEntry } from "@/components/ui/menu";
import type { SessionStatus } from "@/components/app/TerminalView";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { scopeOf, toggleGlobalBroadcast, toggleTabBroadcast, useBroadcast } from "@/lib/broadcast";
import { t } from "@/lib/i18n";
import { appear } from "@/lib/motion";
import type { Axis, PaneState } from "@/lib/panes";
import { shortcutHint } from "@/lib/shortcutTable";
import { hostColor, type Host } from "@/lib/backend";
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
  serialHosts,
  onOpenHost,
  onNewSerial,
  onToggleFiles,
  onSplit,
}: {
  tabs: Tab[];
  activeKey: string | null;
  onSelect: (key: string) => void;
  onClose: (key: string) => void;
  onNew: () => void;
  /** Saved serial ports, offered in the new-tab menu. */
  serialHosts: Host[];
  onOpenHost: (host: Host) => void;
  onNewSerial: () => void;
  onToggleFiles: () => void;
  onSplit: (axis: Axis) => void;
}) {
  const activeTab = tabs.find((tab) => tab.key === activeKey);
  const broadcast = useBroadcast();
  const tabBroadcast = activeTab ? broadcast.tabs.has(activeTab.key) : false;
  // One Tab stop for the strip; arrows move and select (K7), Delete closes.
  const tabKeys = (event: KeyboardEvent, key: string) => {
    const index = tabs.findIndex((tab) => tab.key === key);
    const to =
      event.key === "ArrowRight" ? index + 1 : event.key === "ArrowLeft" ? index - 1 : event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : null;
    if (event.key === "Delete") onClose(key);
    else if (to !== null && tabs.length > 0) {
      const next = tabs[(to + tabs.length) % tabs.length];
      onSelect(next.key);
      window.requestAnimationFrame(() => document.querySelector<HTMLElement>(`[data-tab="${next.key}"]`)?.focus());
    } else return;
    event.preventDefault();
  };
  return (
    <div className="material-bar flex h-11 shrink-0 items-center gap-1 border-b px-2">
      <div role="tablist" className="flex min-w-0 items-center gap-1 overflow-x-auto">
        <AnimatePresence initial={false}>
        {tabs.map((tab) => {
          const active = tab.key === activeKey;
          return (
            <motion.div
              key={tab.key}
              {...appear}
              className={cn(
                "group relative flex h-8 max-w-52 shrink-0 items-center rounded-sm",
                active ? "bg-bg-elevated text-label-1 shadow-1 ring-1 ring-separator" : "text-label-2 hover:bg-fill-1",
              )}
            >
              <button
                type="button"
                role="tab"
                aria-selected={active}
                data-tab={tab.key}
                onClick={() => onSelect(tab.key)}
                onAuxClick={(event) => event.button === 1 && onClose(tab.key)}
                tabIndex={active ? 0 : -1}
                onKeyDown={(event) => tabKeys(event, tab.key)}
                className="relative flex h-full min-w-0 items-center gap-2 pl-3 text-callout"
              >
                <TabIcon tab={tab} />
                <span className="truncate" title={tab.title}>{tab.title}</span>
                {tab.hostId !== null && tab.status.state === "connected" && tab.status.engine && (
                  <EngineBadge engine={tab.status.engine.engine} reason={tab.status.engine.reason} />
                )}
                {scopeOf(broadcast, tab.key) && (
                  <Radio className="size-4 shrink-0 text-warning" strokeWidth={1.5} aria-label={t.broadcastingTab} />
                )}
              </button>
              <Tooltip label={`${t.closeTab} (${shortcutHint("close-tab")})`}>
                <button
                  type="button"
                  tabIndex={-1}
                  aria-label={t.closeTab}
                  onClick={() => onClose(tab.key)}
                  className={cn(
                    "relative mx-1 flex size-6 items-center justify-center rounded-xs text-label-3 transition-opacity duration-micro hover:bg-fill-2 hover:text-label-1",
                    !active && "opacity-0 group-hover:opacity-100 focus-visible:opacity-100",
                  )}
                >
                  <X className="size-4" strokeWidth={1.5} />
                </button>
              </Tooltip>
            </motion.div>
          );
        })}
        </AnimatePresence>
      </div>
      <NewTabMenu serialHosts={serialHosts} onNew={onNew} onOpenHost={onOpenHost} onNewSerial={onNewSerial} />
      {activeTab && (
        <div className="ml-auto flex items-center gap-0.5">
          <Tooltip label={`${t.splitRight} (${shortcutHint("split-right")})`}>
            <Button variant="quiet" size="icon" aria-label={t.splitRight} onClick={() => onSplit("row")}>
              <Columns2 />
            </Button>
          </Tooltip>
          <Tooltip label={`${t.splitDown} (${shortcutHint("split-down")})`}>
            <Button variant="quiet" size="icon" aria-label={t.splitDown} onClick={() => onSplit("col")}>
              <Rows2 />
            </Button>
          </Tooltip>
          <Tooltip label={`${t.broadcastTab} (${shortcutHint("toggle-broadcast")})`}>
            <Button
              variant="quiet"
              size="icon"
              aria-label={t.broadcastTab}
              aria-pressed={tabBroadcast}
              onClick={() => toggleTabBroadcast(activeTab.key)}
              className={cn(tabBroadcast && "bg-accent-soft text-(--accent) hover:bg-accent-soft hover:text-(--accent)")}
            >
              <Radio />
            </Button>
          </Tooltip>
          <Tooltip label={t.broadcastAll}>
            <Button
              variant="quiet"
              size="icon"
              aria-label={t.broadcastAll}
              aria-pressed={broadcast.global}
              onClick={toggleGlobalBroadcast}
              className={cn(broadcast.global && "bg-accent-soft text-(--accent) hover:bg-accent-soft hover:text-(--accent)")}
            >
              <RadioTower />
            </Button>
          </Tooltip>
        </div>
      )}
      {activeTab?.hostId && (
        <Tooltip label={`${t.toggleFiles} (${shortcutHint("toggle-files")})`}>
          <Button
            variant="quiet"
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

/** The plus button: a local terminal, a saved serial port or a new one. */
function NewTabMenu({
  serialHosts,
  onNew,
  onOpenHost,
  onNewSerial,
}: {
  serialHosts: Host[];
  onNew: () => void;
  onOpenHost: (host: Host) => void;
  onNewSerial: () => void;
}) {
  const items: MenuEntry[] = [
    { key: "local", label: t.newTab, icon: <SquareTerminal />, shortcut: shortcutHint("new-local"), onSelect: onNew },
    ...(serialHosts.length > 0 ? (["separator"] as MenuEntry[]) : []),
    ...serialHosts.map((host): MenuEntry => ({ key: host.id, label: host.name, icon: <Cable />, onSelect: () => onOpenHost(host) })),
    "separator",
    { key: "new-serial", label: t.newSerialHost, icon: <Plus />, onSelect: onNewSerial },
  ];
  return <Menu label={t.newTabMenu} icon={<Plus />} size="icon" align="start" items={items} />;
}

/** Which SSH engine runs the tab, with the reason on hover. */
function EngineBadge({ engine, reason }: { engine: "openssh" | "russh"; reason: string | null }) {
  const name = engine === "russh" ? t.engineBuiltin : t.engineOpenssh;
  return (
    <Tooltip label={reason ? `${name}: ${reason}` : name}>
      <span
        role="img"
        aria-label={`${t.sshEngineLabel}: ${name}`}
        className="shrink-0 rounded-xs bg-fill-1 px-1 text-callout text-label-2"
      >
        {engine === "russh" ? t.engineBadgeBuiltin : t.engineBadgeOpenssh}
      </span>
    </Tooltip>
  );
}

/** Status is shown as an icon with an accessible label, never colour alone. */
function TabIcon({ tab }: { tab: Tab }) {
  const icon = "size-4 shrink-0";
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
