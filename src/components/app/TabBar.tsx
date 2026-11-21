import { Activity, Bell, Cable, Columns2, FolderOpen, LoaderCircle, Minimize2, Plus, Radio, Rows2, SquareTerminal, Unplug, X } from "lucide-react";
import { useCallback, useRef, useState, type KeyboardEvent } from "react";
import { AnimatePresence, motion } from "motion/react";
import { ContextMenu, Menu, type MenuEntry } from "@/components/ui/menu";
import type { SessionStatus } from "@/components/app/TerminalView";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { EnvBadge } from "@/components/app/EnvBadge";
import { useBroadcastGuard } from "@/components/app/useBroadcastGuard";
import { scopeOf, useBroadcast } from "@/lib/broadcast";
import { t } from "@/lib/i18n";
import { exitTitle } from "@/lib/terminalSession";
import { appear } from "@/lib/motion";
import type { Axis, PaneState } from "@/lib/panes";
import { shortcutHint } from "@/lib/shortcutTable";
import { hostColor, type Host, type HostEnvironment } from "@/lib/backend";
import { cn } from "@/lib/utils";

/** The drag data type of a tab being reordered. */
const TAB_DRAG = "application/x-anixops-tab";

export interface Tab {
  key: string;
  /** null for a local shell. */
  hostId: string | null;
  title: string;
  color: string | null;
  /** The host's environment, filled in by the app from the saved host; absent for a local shell. */
  environment?: HostEnvironment | null;
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
  /** The server monitor beside the terminal (SSH hosts only). */
  monitorOpen: boolean;
}

export function TabBar({
  tabs,
  activeKey,
  onSelect,
  onClose,
  onCloseMany,
  onDuplicate,
  onRename,
  onMove,
  onNew,
  serialHosts,
  onOpenHost,
  onNewSerial,
  onToggleFiles,
  monitorAvailable,
  onToggleMonitor,
  onSplit,
  onExitFocus,
}: {
  tabs: Tab[];
  activeKey: string | null;
  onSelect: (key: string) => void;
  onClose: (key: string) => void;
  /** Close several tabs at once; `keep` stays open and becomes active if the active one closes. */
  onCloseMany: (keys: string[], keep: string) => void;
  /** Opens another session like this tab's (same host, or a local shell). */
  onDuplicate: (key: string) => void;
  /** Gives a tab a name of its own (empty keeps the current one). */
  onRename: (key: string, title: string) => void;
  /** Moves tab `from` to where tab `to` is. */
  onMove: (from: string, to: string) => void;
  onNew: () => void;
  /** Saved serial ports, offered in the new-tab menu. */
  serialHosts: Host[];
  onOpenHost: (host: Host) => void;
  onNewSerial: () => void;
  onToggleFiles: () => void;
  /** The active tab is an SSH host the monitor can read. */
  monitorAvailable: boolean;
  onToggleMonitor: () => void;
  onSplit: (axis: Axis) => void;
  /** Shown in focus mode, where the bar is all that is left of the window's chrome. */
  onExitFocus?: () => void;
}) {
  const activeTab = tabs.find((tab) => tab.key === activeKey);
  const broadcast = useBroadcast();
  // The right-click menu of a tab: which tab, and where.
  const [menu, setMenu] = useState<{ key: string; at: { x: number; y: number } } | null>(null);
  const closeMenu = useCallback(() => setMenu(null), []);
  // The tab whose name is being edited in place.
  const [renaming, setRenamingState] = useState<string | null>(null);
  const renamingRef = useRef<string | null>(null);
  const setRenaming = (key: string | null) => {
    renamingRef.current = key;
    setRenamingState(key);
  };
  const finishRename = (key: string, value: string | null) => {
    // Escape, then the blur of the field going away: only the first counts.
    if (renamingRef.current !== key) return;
    setRenaming(null);
    if (value !== null && value.trim()) onRename(key, value.trim());
  };
  const menuIndex = menu ? tabs.findIndex((tab) => tab.key === menu.key) : -1;
  const menuItems: MenuEntry[] = menu
    ? [
        { key: "duplicate", label: t.duplicateTab, onSelect: () => onDuplicate(menu.key) },
        { key: "rename", label: t.renameTab, onSelect: () => setRenaming(menu.key) },
        "separator",
        { key: "close", label: t.closeTab, shortcut: shortcutHint("close-tab"), onSelect: () => onClose(menu.key) },
        {
          key: "close-others",
          label: t.closeOtherTabs,
          disabled: tabs.length < 2,
          onSelect: () => onCloseMany(tabs.filter((tab) => tab.key !== menu.key).map((tab) => tab.key), menu.key),
        },
        {
          key: "close-right",
          label: t.closeTabsToRight,
          disabled: menuIndex < 0 || menuIndex === tabs.length - 1,
          onSelect: () => onCloseMany(tabs.slice(menuIndex + 1).map((tab) => tab.key), menu.key),
        },
      ]
    : [];
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
    // A container: in a narrow window the labelled buttons drop their words
    // (icon, tooltip and accessible name stay) so the tabs keep their room.
    <div className="@container material-bar flex h-11 shrink-0 items-center gap-1 border-b px-2">
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
                // Drag a tab along the strip to reorder it.
                draggable
                onDragStart={(event) => {
                  event.dataTransfer.setData(TAB_DRAG, tab.key);
                  event.dataTransfer.effectAllowed = "move";
                }}
                onDragOver={(event) => {
                  if (!event.dataTransfer.types.includes(TAB_DRAG)) return;
                  event.preventDefault();
                  event.dataTransfer.dropEffect = "move";
                }}
                onDrop={(event) => {
                  const from = event.dataTransfer.getData(TAB_DRAG);
                  if (!from) return;
                  event.preventDefault();
                  if (from !== tab.key) onMove(from, tab.key);
                }}
                onClick={() => onSelect(tab.key)}
                onAuxClick={(event) => event.button === 1 && onClose(tab.key)}
                onContextMenu={(event) => {
                  event.preventDefault();
                  setMenu({ key: tab.key, at: { x: event.clientX, y: event.clientY } });
                }}
                tabIndex={active ? 0 : -1}
                onKeyDown={(event) => tabKeys(event, tab.key)}
                className="relative flex h-full min-w-0 items-center gap-2 pl-3 text-callout"
              >
                <TabIcon tab={tab} />
                {renaming !== tab.key && (
                  <span
                    className="truncate"
                    title={tab.status.remoteTitle ? `${tab.title} — ${tab.status.remoteTitle}` : tab.title}
                    onDoubleClick={() => setRenaming(tab.key)}
                  >
                    {tab.title}
                  </span>
                )}
                {tab.status.bell && !active && (
                  <Bell className="size-3.5 shrink-0 text-warning" strokeWidth={1.5} aria-label={t.bell} />
                )}
                {tab.hostId !== null && tab.status.state === "connected" && tab.status.engine && (
                  <EngineBadge engine={tab.status.engine.engine} reason={tab.status.engine.reason} />
                )}
                {tab.environment && <EnvBadge environment={tab.environment} shrink />}
                {scopeOf(broadcast, tab.key) && (
                  <Radio className="size-4 shrink-0 text-warning" strokeWidth={1.5} aria-label={t.broadcastingTab} />
                )}
              </button>
              {renaming === tab.key && (
                <input
                    aria-label={t.renameTab}
                    defaultValue={tab.title}
                    autoFocus
                    onFocus={(event) => event.currentTarget.select()}
                    onClick={(event) => event.stopPropagation()}
                    onKeyDown={(event) => {
                      event.stopPropagation();
                      if (event.key === "Enter") finishRename(tab.key, event.currentTarget.value);
                      else if (event.key === "Escape") finishRename(tab.key, null);
                    }}
                    onBlur={(event) => finishRename(tab.key, event.currentTarget.value)}
                    className="mr-1 h-6 w-32 min-w-0 rounded-xs bg-bg px-1 text-callout text-label-1 outline-none ring-1 ring-(--accent)"
                  />
              )}
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
          <BroadcastMenu tabKey={activeTab.key} />
        </div>
      )}
      {activeTab?.hostId && monitorAvailable && (
        <Tooltip label={t.toggleMonitor}>
          <Button
            variant="quiet"
            aria-pressed={activeTab.monitorOpen}
            aria-label={t.monitor}
            onClick={onToggleMonitor}
            className={cn(BAR_LABEL_PAD, activeTab.monitorOpen && "bg-accent-soft text-(--accent) hover:bg-accent-soft hover:text-(--accent)")}
          >
            <Activity />
            <span className={BAR_LABEL}>{t.monitor}</span>
          </Button>
        </Tooltip>
      )}
      {activeTab?.hostId && (
        <Tooltip label={`${t.toggleFiles} (${shortcutHint("toggle-files")})`}>
          <Button
            variant="quiet"
            aria-pressed={activeTab.filesOpen}
            aria-label={t.files}
            onClick={onToggleFiles}
            className={cn(BAR_LABEL_PAD, activeTab.filesOpen && "bg-accent-soft text-(--accent) hover:bg-accent-soft hover:text-(--accent)")}
          >
            <FolderOpen />
            <span className={BAR_LABEL}>{t.files}</span>
          </Button>
        </Tooltip>
      )}
      {onExitFocus && (
        <Tooltip label={`${t.exitFocusMode} (${shortcutHint("toggle-focus")})`}>
          <Button variant="quiet" size="icon" aria-label={t.exitFocusMode} onClick={onExitFocus}>
            <Minimize2 />
          </Button>
        </Tooltip>
      )}
      <ContextMenu label={t.tabMenu} at={menu?.at ?? null} items={menuItems} onClose={closeMenu} />
    </div>
  );
}

/** Words on the bar's buttons show only when the bar is at least 42rem wide;
 *  without them the button is as wide as an icon button. */
const BAR_LABEL = "hidden @2xl:inline";
const BAR_LABEL_PAD = "max-@2xl:w-9 max-@2xl:px-0";

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
    const label = tab.status.error ? t.sessionFailed : exitTitle(tab.status);
    return <Unplug className={cn(icon, "text-label-3")} strokeWidth={1.5} aria-label={label} />;
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

/** One labelled control for broadcast input: a menu that says what each
 *  scope does and how many terminals it reaches. Off while there is only
 *  one terminal, since there is nowhere else to send to. */
function BroadcastMenu({ tabKey }: { tabKey: string }) {
  const broadcast = useBroadcast();
  const guard = useBroadcastGuard();
  const scope = scopeOf(broadcast, tabKey);
  const inTab = broadcast.panes.get(tabKey)?.length ?? 0;
  const total = [...broadcast.panes.values()].reduce((sum, ids) => sum + ids.length, 0);
  const items: MenuEntry[] = [
    { key: "off", label: t.broadcastOff, hint: t.broadcastOffHint, checked: scope === null, onSelect: () => void guard.setScope(tabKey, null) },
    {
      key: "tab",
      label: t.broadcastTabItem.replace("{n}", String(inTab)),
      hint: t.broadcastTabHint,
      shortcut: shortcutHint("toggle-broadcast"),
      checked: scope === "tab",
      disabled: inTab < 2 && scope !== "tab",
      onSelect: () => void guard.setScope(tabKey, "tab"),
    },
    {
      key: "global",
      label: t.broadcastAllItem.replace("{n}", String(total)),
      hint: t.broadcastAllHint,
      checked: scope === "global",
      disabled: total < 2 && scope !== "global",
      onSelect: () => void guard.setScope(tabKey, "global"),
    },
  ];
  return (
    <Menu
      label={t.broadcastMenu}
      tooltip={total < 2 && scope === null ? t.broadcastNeedsTwo : t.broadcastMenu}
      icon={<Radio />}
      text={<span className={BAR_LABEL}>{scope ? t.broadcastOnShort : t.broadcast}</span>}
      variant="quiet"
      wide
      disabled={total < 2 && scope === null}
      className={cn(BAR_LABEL_PAD, scope && "bg-warning-soft text-warning hover:bg-warning-soft hover:text-warning")}
      items={items}
    />
  );
}
