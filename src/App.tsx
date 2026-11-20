import { AnimatePresence, MotionConfig } from "motion/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { AskpassDialog } from "@/components/app/AskpassDialog";
import { HostKeyDialog } from "@/components/app/HostKeyDialog";
import { VaultUnlockDialog } from "@/components/app/VaultUnlockDialog";
import { CommandPalette, type PaneAction } from "@/components/app/CommandPalette";
import { DeepLinkHandler } from "@/components/app/DeepLinkHandler";
import { FilesPane } from "@/components/app/FilesPane";
import { MonitorPane } from "@/components/app/MonitorPane";
import { BroadcastBar } from "@/components/app/PaneOverlays";
import { monitorOpenByDefault, rememberMonitorOpen } from "@/lib/monitorPrefs";
import { rememberSidebarHidden, sidebarHiddenByDefault } from "@/lib/sidebarPrefs";
import { ForwardsPanel } from "@/components/app/ForwardsPanel";
import { HostEditor } from "@/components/app/HostEditor";
import { HostList } from "@/components/app/HostList";
import { ImportDialog } from "@/components/app/ImportDialog";
import { IconRail, type NavKey } from "@/components/app/IconRail";
import { EmptyWorkspace } from "@/components/app/Placeholders";
import { SettingsPanel } from "@/components/app/SettingsPanel";
import { SnippetsPanel } from "@/components/app/SnippetsPanel";
import { TeamVaults } from "@/components/app/TeamVaults";
import { TabBar, type Tab } from "@/components/app/TabBar";
import { PaneArea } from "@/components/app/PaneArea";
import type { SessionStatus } from "@/components/app/TerminalView";
import { ShortcutList } from "@/components/app/ShortcutList";
import { ConfirmProvider } from "@/components/ui/confirm";
import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/form";
import { TooltipProvider } from "@/components/ui/tooltip";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  isTauri,
  listSessions,
  loadHosts,
  onHostsChanged,
  setFavorite,
  sshStatus,
  deleteHost,
  type Host,
  type HostFields,
  type HostsSnapshot,
  type SshStatus,
} from "@/lib/backend";
import { adhocId, formatTarget, parseAdhocId, type Target } from "@/lib/quickConnect";
import { participants, scopeOf, setBroadcastScope, setPanes, toggleGlobalBroadcast, toggleTabBroadcast, useBroadcast } from "@/lib/broadcast";
import { t } from "@/lib/i18n";
import { serialHosts } from "@/lib/serialUi";
import { loadLayout, planRestore, saveLayout, setWindowLabel } from "@/lib/layoutStore";
import {
  closeInState,
  cycleFocus,
  deserialize,
  focusPane,
  initialState,
  leaves,
  moveFocus,
  resizeSplit,
  serialize,
  splitFocused,
  toggleMaximise,
  type Axis,
  type Direction,
  type PaneState,
} from "@/lib/panes";
import { loadSettings } from "@/lib/settings";
import { FirstRunPrompt } from "@/components/app/FirstRunPrompt";
import { appShortcut, inTerminal, type AppShortcut } from "@/lib/shortcuts";
import { terminalFor } from "@/lib/terminals";

let nextTab = 1;
let nextPane = 1;
const newPaneId = () => `pane-${nextPane++}`;

const connecting: SessionStatus = { state: "connecting" };

/** A tab's status is that of its focused pane. */
function withStatus(tab: Tab): Tab {
  return { ...tab, status: tab.paneStatus[tab.panes.focused] ?? connecting };
}

function newTab(hostId: string | null, title: string, color: string | null, panes?: PaneState): Tab {
  const state = panes ?? initialState(newPaneId());
  return {
    key: `tab-${nextTab++}`,
    hostId,
    title,
    color,
    status: connecting,
    panes: state,
    paneStatus: {},
    restored: {},
    filesOpen: false,
    filesMounted: false,
    monitorOpen: hostId !== null && monitorOpenByDefault(),
  };
}

const focusShortcuts: Partial<Record<AppShortcut, Direction>> = {
  "focus-left": "left",
  "focus-right": "right",
  "focus-up": "up",
  "focus-down": "down",
};

export function App() {
  const [nav, setNavKey] = useState<NavKey>("hosts");
  // The sidebar can be hidden so the terminal gets the whole window; going to
  // a section shows it again, and choosing the section that is open hides it.
  const [sidebarHidden, setSidebarHidden] = useState(sidebarHiddenByDefault);
  const hideSidebar = useCallback((hidden: boolean) => {
    rememberSidebarHidden(hidden);
    setSidebarHidden(hidden);
  }, []);
  const setNav = useCallback(
    (key: NavKey) => {
      setNavKey(key);
      hideSidebar(false);
    },
    [hideSidebar],
  );
  const toggleSidebar = useCallback(() => hideSidebar(!sidebarHidden), [hideSidebar, sidebarHidden]);
  const chooseNav = useCallback(
    (key: NavKey) => (key === nav ? hideSidebar(!sidebarHidden) : setNav(key)),
    [hideSidebar, nav, setNav, sidebarHidden],
  );
  const [snapshot, setSnapshot] = useState<HostsSnapshot>({ hosts: [], groups: [], warnings: [] });
  const [loadError, setLoadError] = useState<string | null>(null);
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  // undefined: closed; null: a new host.
  const [editing, setEditing] = useState<Host | null | undefined>(undefined);
  /** A new host being created starts as a serial port. */
  const [newSerial, setNewSerial] = useState(false);
  /** Values for the host being created from an unsaved connection. */
  const [prefill, setPrefill] = useState<{ name: string; fields: HostFields } | undefined>(undefined);
  /** Tabs of unsaved connections that no longer offer "save as host". */
  const [savedAdhoc, setSavedAdhoc] = useState<Set<string>>(new Set());
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [importing, setImporting] = useState(false);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [status, setStatus] = useState<SshStatus | null>(null);

  const checkSsh = useCallback(() => {
    setStatus(null);
    sshStatus().then(setStatus).catch(console.error);
  }, []);

  useEffect(() => {
    loadSettings().catch((reason) => setLoadError(String(reason)));
    checkSsh();
  }, [checkSsh]);
  const searchRef = useRef<HTMLInputElement>(null);

  const reload = useCallback(() => {
    loadHosts()
      .then((next) => {
        setSnapshot(next);
        setLoadError(null);
      })
      .catch((reason) => setLoadError(String(reason)));
  }, []);

  useEffect(reload, [reload]);
  // Hosts saved or deleted from another window refresh this list too.
  useEffect(() => onHostsChanged(reload), [reload]);

  const toggleFavorite = useCallback(
    (host: Host) => {
      setFavorite(host.id, !host.favorite)
        .then(reload)
        .catch((reason) => setLoadError(String(reason)));
    },
    [reload],
  );

  const removeHost = useCallback(
    (host: Host) => {
      deleteHost(host.id)
        .then(reload)
        .catch((reason) => setLoadError(String(reason)));
    },
    [reload],
  );

  const addTab = useCallback((hostId: string | null, title: string, color: string | null) => {
    const tab = newTab(hostId, title, color);
    setTabs((current) => [...current, tab]);
    setActiveKey(tab.key);
  }, []);

  // Clicking a host focuses its open tab; a new session needs a closed tab.
  const openHost = useCallback(
    (host: Host) => {
      const existing = tabs.find((tab) => tab.hostId === host.id);
      if (existing) setActiveKey(existing.key);
      else addTab(host.id, host.name, host.color);
    },
    [tabs, addTab],
  );

  // An address typed in the palette or confirmed from a link: no saved host.
  const quickConnect = useCallback(
    (target: Target) => {
      const hostId = adhocId(target);
      const existing = tabs.find((tab) => tab.hostId === hostId);
      if (existing) setActiveKey(existing.key);
      else addTab(hostId, formatTarget(target), null);
    },
    [tabs, addTab],
  );

  const saveAdhoc = useCallback((tab: Tab) => {
    const target = tab.hostId ? parseAdhocId(tab.hostId) : null;
    if (!target) return;
    setPrefill({
      name: target.host,
      fields: {
        alias: null,
        hostname: target.host,
        username: target.user,
        port: target.port,
        proxyJump: null,
        notes: "",
        shellIntegration: false,
      },
    });
    setNewSerial(false);
    setEditing(null);
    setAdhocTab(tab.key);
  }, []);
  const [adhocTab, setAdhocTab] = useState<string | null>(null);

  const openLocal = useCallback(() => addTab(null, t.localTerminal, null), [addTab]);

  const startSerialHost = useCallback(() => {
    setNav("hosts");
    setNewSerial(true);
    setEditing(null);
  }, []);

  // Sessions that outlived a window reload come back with the layout they had,
  // once. Sessions the saved layout does not know come back as tabs of their own.
  const restored = useRef(false);
  const [ready, setReady] = useState(false);
  useEffect(() => {
    if (restored.current) return;
    restored.current = true;
    if (isTauri) setWindowLabel(getCurrentWindow().label);
    Promise.all([loadHosts(), listSessions()])
      .then(([hosts, live]) => {
        const plan = planRestore(loadLayout(), live);
        if (plan.tabs.length === 0) return;
        const byId = new Map(hosts.hosts.map((host) => [host.id, host]));
        const restoredTabs = plan.tabs.map((planned) => {
          const host = planned.hostId ? byId.get(planned.hostId) : undefined;
          const { tree, sessions } = deserialize(planned.tree, newPaneId);
          const order = leaves(tree);
          const state: PaneState = {
            tree,
            focused: order[planned.focused] ?? order[0],
            maximised: planned.maximised === null ? null : (order[planned.maximised] ?? null),
          };
          const title =
            host?.name ??
            (planned.hostId
              ? planned.hostId.replace(/^ssh-config:/, "").replace(/^adhoc:/, "").replace(/:22$/, "")
              : t.localTerminal);
          const tab = newTab(planned.hostId, title, host?.color ?? null, state);
          tab.restored = Object.fromEntries(
            Object.entries(sessions).filter((entry): entry is [string, string] => entry[1] !== null),
          );
          tab.filesOpen = planned.filesOpen && planned.hostId !== null;
          tab.filesMounted = tab.filesOpen;
          return tab;
        });
        // Keep tabs the user opened while this was loading.
        setTabs((current) => [...restoredTabs, ...current]);
        setActiveKey((current) => current ?? restoredTabs[plan.active ?? 0].key);
      })
      .catch((reason) => console.error(reason))
      .finally(() => setReady(true));
  }, []);

  // The daemon's session behind each pane, kept for the saved layout.
  const sessionIds = useRef(new Map<string, string>());
  const persistRef = useRef<() => void>(() => {});
  persistRef.current = () => {
    if (!ready) return;
    saveLayout({
      version: 1,
      active: Math.max(0, tabs.findIndex((tab) => tab.key === activeKey)),
      tabs: tabs.map((tab) => {
        const order = leaves(tab.panes.tree);
        return {
          hostId: tab.hostId,
          filesOpen: tab.filesOpen,
          tree: serialize(tab.panes.tree, (id) => sessionIds.current.get(id) ?? tab.restored[id] ?? null),
          focused: Math.max(0, order.indexOf(tab.panes.focused)),
          maximised: tab.panes.maximised === null ? null : order.indexOf(tab.panes.maximised),
        };
      }),
    });
  };
  useEffect(() => persistRef.current(), [tabs, activeKey, ready]);
  const onSession = useCallback((paneId: string, sessionId: string) => {
    sessionIds.current.set(paneId, sessionId);
    persistRef.current();
  }, []);

  // Broadcast needs to know which panes belong to which tab.
  useEffect(() => {
    setPanes(new Map(tabs.map((tab) => [tab.key, leaves(tab.panes.tree)])));
  }, [tabs]);
  const broadcast = useBroadcast();

  const closeTab = useCallback(
    (key: string) => {
      const closing = tabs.find((tab) => tab.key === key);
      for (const pane of closing ? leaves(closing.panes.tree) : []) {
        terminalFor(pane)?.close();
        sessionIds.current.delete(pane);
      }
      const index = tabs.findIndex((tab) => tab.key === key);
      const next = tabs.filter((tab) => tab.key !== key);
      setTabs(next);
      if (key === activeKey) setActiveKey(next[Math.min(index, next.length - 1)]?.key ?? null);
    },
    [tabs, activeKey],
  );

  // The tab menu's "close other tabs" and "close tabs to the right": `keep` stays open and active.
  const closeTabs = useCallback(
    (keys: string[], keep: string) => {
      const closing = new Set(keys);
      for (const tab of tabs) {
        if (!closing.has(tab.key)) continue;
        for (const pane of leaves(tab.panes.tree)) {
          terminalFor(pane)?.close();
          sessionIds.current.delete(pane);
        }
      }
      setTabs(tabs.filter((tab) => !closing.has(tab.key)));
      if (activeKey === null || closing.has(activeKey)) setActiveKey(keep);
    },
    [tabs, activeKey],
  );

  // Stable so terminals do not restart their sessions when the app re-renders.
  const updateStatus = useCallback((paneId: string, status: SessionStatus) => {
    setTabs((current) =>
      current.map((tab) =>
        leaves(tab.panes.tree).includes(paneId)
          ? withStatus({ ...tab, paneStatus: { ...tab.paneStatus, [paneId]: status } })
          : tab,
      ),
    );
  }, []);

  const updatePanes = useCallback((key: string, change: (state: PaneState) => PaneState) => {
    setTabs((current) =>
      current.map((tab) => (tab.key === key ? withStatus({ ...tab, panes: change(tab.panes) }) : tab)),
    );
  }, []);

  const splitActive = useCallback(
    (axis: Axis) => {
      if (activeKey) updatePanes(activeKey, (state) => splitFocused(state, axis, newPaneId(), `split-${nextPane++}`));
    },
    [activeKey, updatePanes],
  );

  const closePane = useCallback(
    (key: string, paneId: string) => {
      const tab = tabs.find((candidate) => candidate.key === key);
      if (!tab) return;
      if (leaves(tab.panes.tree).length < 2) return closeTab(key);
      terminalFor(paneId)?.close();
      sessionIds.current.delete(paneId);
      setTabs((current) =>
        current.map((candidate) => {
          if (candidate.key !== key) return candidate;
          const panes = closeInState(candidate.panes, paneId);
          if (!panes) return candidate;
          const { [paneId]: _gone, ...paneStatus } = candidate.paneStatus;
          return withStatus({ ...candidate, panes, paneStatus });
        }),
      );
    },
    [tabs, closeTab],
  );

  const setFilesOpen = useCallback((key: string, filesOpen: boolean) => {
    setTabs((current) => current.map((tab) => (tab.key === key && tab.hostId ? { ...tab, filesOpen, filesMounted: true } : tab)));
  }, []);

  const setMonitorOpen = useCallback((key: string, monitorOpen: boolean) => {
    rememberMonitorOpen(monitorOpen);
    setTabs((current) => current.map((tab) => (tab.key === key && tab.hostId ? { ...tab, monitorOpen } : tab)));
  }, []);

  const toggleMonitor = useCallback(() => {
    const tab = tabs.find((candidate) => candidate.key === activeKey);
    if (tab) setMonitorOpen(tab.key, !tab.monitorOpen);
  }, [tabs, activeKey, setMonitorOpen]);

  const toggleFiles = useCallback(() => {
    const tab = tabs.find((candidate) => candidate.key === activeKey);
    if (tab) setFilesOpen(tab.key, !tab.filesOpen);
  }, [tabs, activeKey, setFilesOpen]);

  const activeTab = tabs.find((tab) => tab.key === activeKey);
  /** The SSH host a tab's monitor reads; null for local shells and serial ports. */
  const monitorHost = (hostId: string | null): Host | null => {
    const host = hostId ? snapshot.hosts.find((candidate) => candidate.id === hostId) : undefined;
    return host && !host.fields.serial ? host : null;
  };
  const activePane = activeTab?.panes.focused ?? null;

  const paneAction = useCallback(
    (action: PaneAction) => {
      if (!activeTab) return;
      if (action === "split-right") splitActive("row");
      else if (action === "split-down") splitActive("col");
      else if (action === "close-pane") closePane(activeTab.key, activeTab.panes.focused);
      else if (action === "focus-next") updatePanes(activeTab.key, (state) => cycleFocus(state, 1));
      else if (action === "focus-previous") updatePanes(activeTab.key, (state) => cycleFocus(state, -1));
      else if (action === "toggle-maximise") updatePanes(activeTab.key, toggleMaximise);
      else if (action === "toggle-broadcast-tab") toggleTabBroadcast(activeTab.key);
      else toggleGlobalBroadcast();
    },
    [activeTab, splitActive, closePane, updatePanes],
  );

  const focusSearch = useCallback(() => {
    setNav("hosts");
    window.requestAnimationFrame(() => searchRef.current?.focus());
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const shortcut = appShortcut(event);
      const here = inTerminal(event);
      if (shortcut === "search-hosts") setPaletteOpen(true);
      else if (shortcut === "shortcut-list") setShortcutsOpen(true);
      else if (shortcut === "settings") setNav("settings");
      else if (shortcut === "go-hosts") setNav("hosts");
      else if (shortcut === "go-forwards") setNav("forwards");
      else if (shortcut === "go-snippets") setNav("snippets");
      else if (shortcut === "go-team") setNav("team");
      else if (shortcut === "import") setImporting(true);
      else if (shortcut === "new-host") setEditing(null);
      // In a terminal "find" is the terminal's own; elsewhere it finds a host.
      else if (shortcut === "find" && !here) focusSearch();
      else if (shortcut === "find") return;
      else if ((shortcut === "next-tab" || shortcut === "prev-tab") && tabs.length > 1 && activeKey) {
        const index = tabs.findIndex((tab) => tab.key === activeKey);
        const step = shortcut === "next-tab" ? 1 : -1;
        setActiveKey(tabs[(index + step + tabs.length) % tabs.length].key);
      } else if (shortcut === "toggle-sidebar") toggleSidebar();
      else if (shortcut === "new-local") openLocal();
      // Closing acts on the focused pane first; the last pane closes the tab.
      else if (shortcut === "close-tab" && activeTab) closePane(activeTab.key, activeTab.panes.focused);
      else if (shortcut === "toggle-files") toggleFiles();
      else if (shortcut === "split-right") paneAction("split-right");
      else if (shortcut === "split-down") paneAction("split-down");
      else if (shortcut === "toggle-broadcast") paneAction("toggle-broadcast-tab");
      else if (shortcut === "toggle-maximise") paneAction("toggle-maximise");
      else if (shortcut && focusShortcuts[shortcut]) {
        const direction = focusShortcuts[shortcut];
        if (activeKey && direction) updatePanes(activeKey, (state) => moveFocus(state, direction));
      } else return;
      event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [activeKey, activeTab, closePane, focusSearch, openLocal, paneAction, setNav, tabs, toggleFiles, toggleSidebar, updatePanes]);

  return (
    <MotionConfig reducedMotion="user">
    <TooltipProvider delayDuration={500}>
    <ConfirmProvider>
      <div className="flex h-full">
        <FirstRunPrompt />
        <IconRail active={nav} sidebarHidden={sidebarHidden} onChange={chooseNav} />
        {/* Switching sections is immediate in a desktop window (M9). */}
        <div key={nav} className="flex shrink-0" hidden={sidebarHidden}>
        {nav === "hosts" ? (
          <HostList
            hosts={snapshot.hosts}
            groups={snapshot.groups}
            selectedId={activeTab?.hostId ?? null}
            onOpen={openHost}
            onEdit={(host) => {
              setNewSerial(false);
              setEditing(host);
            }}
            onToggleFavorite={toggleFavorite}
            onDelete={removeHost}
            onImport={() => setImporting(true)}
            onNewSerial={startSerialHost}
            searchRef={searchRef}
          />
        ) : nav === "snippets" ? (
          <SnippetsPanel activeKey={activePane} />
        ) : nav === "forwards" ? (
          <ForwardsPanel hosts={snapshot.hosts} />
        ) : nav === "team" ? (
          <TeamVaults />
        ) : (
          <SettingsPanel status={status} onRecheck={checkSsh} onDataChanged={reload} />
        )}
        </div>
        <main className="flex min-w-0 flex-1 flex-col bg-bg">
          {tabs.length > 0 && (
            <TabBar
              tabs={tabs}
              activeKey={activeKey}
              onSelect={setActiveKey}
              onClose={closeTab}
              onCloseMany={closeTabs}
              onNew={openLocal}
              serialHosts={serialHosts(snapshot.hosts)}
              onOpenHost={openHost}
              onNewSerial={startSerialHost}
              onToggleFiles={toggleFiles}
              monitorAvailable={monitorHost(activeTab?.hostId ?? null) !== null}
              onToggleMonitor={toggleMonitor}
              onSplit={splitActive}
            />
          )}
          {loadError && (
            <Notice className="m-4">
              {t.loadFailed}: {loadError}
            </Notice>
          )}
          {activeTab?.hostId?.startsWith("adhoc:") &&
            activeTab.status.state === "connected" &&
            !savedAdhoc.has(activeTab.key) && (
              <div className="flex items-center gap-3 border-b px-4 py-2">
                <p className="min-w-0 flex-1 truncate text-callout text-label-2">{t.adhocNotSaved}</p>
                <Button variant="quiet" size="sm" onClick={() => saveAdhoc(activeTab)}>
                  {t.saveAsHost}
                </Button>
              </div>
            )}
          {activeTab && scopeOf(broadcast, activeTab.key) && (
            <BroadcastBar
              scope={scopeOf(broadcast, activeTab.key)!}
              count={participants(broadcast, broadcast.panes, activeTab.key).length}
              onStop={() => setBroadcastScope(activeTab.key, null)}
            />
          )}
          <div className="min-h-0 flex-1">
            {tabs.map((tab) => (
              <div key={tab.key} className="flex h-full" hidden={tab.key !== activeKey}>
                {(() => {
                  const host = monitorHost(tab.hostId);
                  return host && tab.monitorOpen ? (
                    <MonitorPane
                      host={host}
                      visible
                      active={tab.key === activeKey}
                      onClose={() => setMonitorOpen(tab.key, false)}
                    />
                  ) : null;
                })()}
                <PaneArea
                  tabKey={tab.key}
                  hostId={tab.hostId}
                  state={tab.panes}
                  restored={tab.restored}
                  active={tab.key === activeKey}
                  broadcast={broadcast}
                  onFocus={(paneId) => updatePanes(tab.key, (state) => focusPane(state, paneId))}
                  onResize={(splitId, ratio) =>
                    updatePanes(tab.key, (state) => ({ ...state, tree: resizeSplit(state.tree, splitId, ratio) }))
                  }
                  onClosePane={(paneId) => closePane(tab.key, paneId)}
                  onToggleMaximise={() => updatePanes(tab.key, toggleMaximise)}
                  onStatus={updateStatus}
                  onSession={onSession}
                />
                {tab.hostId && tab.filesMounted && (
                  <FilesPane
                    hostId={tab.hostId}
                    visible={tab.filesOpen}
                    active={tab.key === activeKey && tab.filesOpen}
                    onClose={() => setFilesOpen(tab.key, false)}
                  />
                )}
              </div>
            ))}
            {!activeTab && <EmptyWorkspace onOpenLocal={openLocal} status={status} />}
          </div>
        </main>
      </div>
      <AnimatePresence>
      {editing !== undefined && (
        <HostEditor
          host={editing}
          groups={snapshot.groups}
          serial={newSerial}
          prefill={prefill}
          onCancel={() => {
            setEditing(undefined);
            setPrefill(undefined);
          }}
          onDone={() => {
            setEditing(undefined);
            if (prefill && adhocTab) setSavedAdhoc((current) => new Set(current).add(adhocTab));
            setPrefill(undefined);
            reload();
          }}
        />
      )}
      </AnimatePresence>
      <AnimatePresence>
      {importing && (
        <ImportDialog groups={snapshot.groups} onChanged={reload} onClose={() => setImporting(false)} />
      )}
      </AnimatePresence>
      <AnimatePresence>
      {paletteOpen && (
        <CommandPalette
          hosts={snapshot.hosts}
          activeKey={activePane}
          activeHostTab={Boolean(activeTab?.hostId)}
          onClose={() => setPaletteOpen(false)}
          onOpenHost={openHost}
          onQuickConnect={quickConnect}
          onOpenLocal={openLocal}
          onNewHost={() => {
            setPrefill(undefined);
            setNewSerial(false);
            setEditing(null);
          }}
          onImport={() => setImporting(true)}
          onToggleFiles={toggleFiles}
          onToggleMonitor={monitorHost(activeTab?.hostId ?? null) ? toggleMonitor : undefined}
          onPaneAction={paneAction}
          paneCount={activeTab ? leaves(activeTab.panes.tree).length : 0}
          maximised={activeTab?.panes.maximised != null}
          tabBroadcast={activeTab ? broadcast.tabs.has(activeTab.key) : false}
          globalBroadcast={broadcast.global}
          onNav={setNav}
          onShortcutList={() => setShortcutsOpen(true)}
        />
      )}
      </AnimatePresence>
      <DeepLinkHandler onOpenHost={openHost} onQuickConnect={quickConnect} onError={setLoadError} />
      <AskpassDialog />
      <HostKeyDialog />
      <VaultUnlockDialog />
      <AnimatePresence>{shortcutsOpen && <ShortcutList onClose={() => setShortcutsOpen(false)} />}</AnimatePresence>
    </ConfirmProvider>
    </TooltipProvider>
    </MotionConfig>
  );
}
