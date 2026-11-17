import { Cable, Columns2, FolderOpen, LoaderCircle, Plus, Radio, RadioTower, Rows2, SquareTerminal, Unplug, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { MenuItem } from "@/components/app/HostList";
import type { SessionStatus } from "@/components/app/TerminalView";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { scopeOf, toggleGlobalBroadcast, toggleTabBroadcast, useBroadcast } from "@/lib/broadcast";
import { t } from "@/lib/i18n";
import { duration, ease, reflow, slide } from "@/lib/motion";
import type { Axis, PaneState } from "@/lib/panes";
import { shortcutHint, shortcutLabel } from "@/lib/shortcuts";
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
                {tab.hostId !== null && tab.status.state === "connected" && tab.status.engine && (
                  <EngineBadge engine={tab.status.engine.engine} reason={tab.status.engine.reason} />
                )}
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
      <NewTabMenu serialHosts={serialHosts} onNew={onNew} onOpenHost={onOpenHost} onNewSerial={onNewSerial} />
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
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  const choose = (run: () => void) => {
    setOpen(false);
    run();
  };

  return (
    <div
      ref={rootRef}
      className="relative"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          setOpen(false);
        }
      }}
    >
      <Tooltip label={`${t.newTabMenu} (${shortcutLabel("t")}: ${t.newTab})`}>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={t.newTabMenu}
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen((current) => !current)}
        >
          <Plus />
        </Button>
      </Tooltip>
      <AnimatePresence>
        {open && (
          <motion.div
            role="menu"
            aria-label={t.newTabMenu}
            className="absolute top-full left-0 z-dropdown mt-1.5 max-h-80 w-60 origin-top-left overflow-y-auto rounded-sm bg-bg-elevated p-1 shadow-2 ring-1 ring-separator"
            initial={{ opacity: 0, y: -4 }}
            animate={{ opacity: 1, y: 0, transition: { duration: duration.micro, ease: ease.standard } }}
            exit={{ opacity: 0, y: -4, transition: { duration: duration.micro, ease: ease.exit } }}
          >
            <MenuItem icon={<SquareTerminal />} label={t.newTab} onClick={() => choose(onNew)} autoFocus />
            {serialHosts.length > 0 && (
              <p className="px-2 pt-2 pb-1 text-caption font-normal text-label-3">{t.serialSection}</p>
            )}
            {serialHosts.map((host) => (
              <MenuItem key={host.id} icon={<Cable />} label={host.name} onClick={() => choose(() => onOpenHost(host))} />
            ))}
            <MenuItem icon={<Plus />} label={t.newSerialHost} onClick={() => choose(onNewSerial)} />
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

/** Which SSH engine runs the tab, with the reason on hover. */
function EngineBadge({ engine, reason }: { engine: "openssh" | "russh"; reason: string | null }) {
  const name = engine === "russh" ? t.engineBuiltin : t.engineOpenssh;
  return (
    <Tooltip label={reason ? `${name}: ${reason}` : name}>
      <span
        role="img"
        aria-label={`${t.sshEngineLabel}: ${name}`}
        className="shrink-0 rounded-xs bg-fill-1 px-1 text-caption font-normal text-label-2"
      >
        {engine === "russh" ? t.engineBadgeBuiltin : t.engineBadgeOpenssh}
      </span>
    </Tooltip>
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
