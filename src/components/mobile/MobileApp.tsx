import { AnimatePresence, MotionConfig } from "motion/react";
import { useCallback, useEffect, useRef, useState } from "react";
import mark from "@/assets/mark.svg";
import { ShortcutList } from "@/components/app/ShortcutList";
import { AskpassDialog } from "@/components/app/AskpassDialog";
import { HostKeyDialog } from "@/components/app/HostKeyDialog";
import { MobileFiles } from "@/components/mobile/MobileFiles";
import { MobileForwards } from "@/components/mobile/MobileForwards";
import { MobileHostForm } from "@/components/mobile/MobileHostForm";
import { MobileHostList } from "@/components/mobile/MobileHostList";
import { MobileLockScreen } from "@/components/mobile/MobileLockScreen";
import { MobileSettings, MOBILE_SHORTCUTS } from "@/components/mobile/MobileSettings";
import { MobileSync } from "@/components/mobile/MobileSync";
import { MobileTabBar, type MobileTab } from "@/components/mobile/MobileTabBar";
import { MobileTerminal, type TerminalController } from "@/components/mobile/MobileTerminal";
import { MobileVaultSetup } from "@/components/mobile/MobileVaultSetup";
import { ConfirmProvider } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { TooltipProvider } from "@/components/ui/tooltip";
import { setSessionNotification } from "@/lib/androidBridge";
import {
  loadHosts,
  onHostsChanged,
  vaultBackground,
  vaultForeground,
  type Group,
  type Host,
} from "@/lib/backend";
import { t } from "@/lib/i18n";
import { useKeyboardViewport } from "@/lib/keyboardViewport";
import { useMobilePrefs } from "@/lib/mobilePrefs";
import { vaultGate, watchLifecycle } from "@/lib/mobileVault";
import { appShortcut, inTerminal, type AppShortcut } from "@/lib/shortcuts";
import { useVaultStatus } from "@/lib/useVaultStatus";

/** What is open on a host. The terminal stays mounted while the files are on
 *  top of it, so looking at a file does not end the session. */
interface Open {
  host: Host;
  attempt: number;
  terminal: boolean;
  files: boolean;
}

/** The touch UI of the mobile app: three destinations in a tab bar (hosts,
 *  forwards, sync), and a host opens as a terminal or a file browser. Under the
 *  `md` breakpoint (834 px, guidelines/layout.md) the destinations and the open
 *  host are screens; from `md` up (a tablet) they sit side by side. The app is laid out
 *  to the visual viewport, so the key bar stays above the soft keyboard, and a
 *  hardware keyboard drives the same shortcuts as the desktop where they apply
 *  (guidelines/keyboard.md, K20). */
export function MobileApp() {
  const [hosts, setHosts] = useState<Host[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [groups, setGroups] = useState<Group[]>([]);
  const [tab, setTab] = useState<MobileTab>("hosts");
  const [open, setOpen] = useState<Open | null>(null);
  const [editing, setEditing] = useState<Host | "new" | null>(null);
  const [settings, setSettings] = useState(false);
  const [shortcuts, setShortcuts] = useState(false);
  const viewport = useKeyboardViewport();
  const prefs = useMobilePrefs();
  const controller = useRef<TerminalController | null>(null);
  const vault = useVaultStatus();
  const gate = vaultGate(vault.status);

  const reload = useCallback(() => {
    loadHosts()
      .then((snapshot) => {
        setHosts(snapshot.hosts);
        setGroups(snapshot.groups);
        setLoadError(null);
      })
      .catch((reason) => setLoadError(String(reason)));
  }, []);
  useEffect(reload, [reload]);
  useEffect(() => onHostsChanged(reload), [reload]);

  // Tell the backend when the app leaves and returns, so the vault locks (at
  // once by default); read the state again on return to show the lock screen
  // before anything else.
  useEffect(
    () => watchLifecycle(document, { background: vaultBackground, foreground: vaultForeground }, vault.refresh, window),
    [vault.refresh],
  );

  // A host that was deleted or edited elsewhere (sync) is closed or refreshed.
  useEffect(() => {
    setOpen((current) => {
      if (!current) return current;
      const fresh = hosts.find((host) => host.id === current.host.id);
      return fresh ? { ...current, host: fresh } : hosts.length > 0 ? null : current;
    });
  }, [hosts]);

  const openHost = (host: Host) =>
    setOpen((current) => ({ host, attempt: (current?.attempt ?? 0) + 1, terminal: true, files: false }));
  const openFiles = (host: Host) =>
    setOpen((current) =>
      current?.terminal && current.host.id === host.id
        ? { ...current, files: true }
        : { host, attempt: (current?.attempt ?? 0) + 1, terminal: false, files: true },
    );

  // The optional Android notification while a terminal is open (opt-in).
  const terminalOpen = open?.terminal === true;
  useEffect(() => {
    setSessionNotification(prefs.foregroundService && terminalOpen, t.mobileForegroundTitle, `1 ${t.mobileForegroundText}`);
  }, [prefs.foregroundService, terminalOpen]);
  useEffect(() => () => setSessionNotification(false, "", ""), []);

  // A hardware keyboard (a tablet with a keyboard cover, a phone over Bluetooth).
  const state = useRef({ open, adding: editing !== null, settings, shortcuts });
  state.current = { open, adding: editing !== null, settings, shortcuts };
  useEffect(() => {
    const run = (shortcut: AppShortcut, event: KeyboardEvent): boolean => {
      const current = state.current;
      const overlay = current.adding || current.settings || current.shortcuts;
      switch (shortcut) {
        case "shortcut-list":
          setShortcuts((on) => !on);
          return true;
        case "settings":
          if (!overlay) setSettings(true);
          return !overlay;
        case "search-hosts":
        case "find":
          // Find inside a terminal belongs to the shell and to xterm.js.
          if (shortcut === "find" && inTerminal(event)) return false;
          if (overlay) return false;
          setOpen(null);
          window.requestAnimationFrame(() => document.querySelector<HTMLElement>("[data-host-search]")?.focus());
          return true;
        case "go-hosts":
          if (overlay) return false;
          setOpen(null);
          return true;
        case "new-host":
          if (overlay) return false;
          setEditing("new");
          return true;
        case "close-tab":
          if (overlay || !controller.current) return false;
          controller.current.close();
          return true;
        case "copy":
          if (overlay || !controller.current) return false;
          controller.current.copy();
          return true;
        case "paste":
          if (overlay || !controller.current) return false;
          controller.current.paste();
          return true;
        default:
          return false;
      }
    };
    const onKey = (event: KeyboardEvent) => {
      const shortcut = appShortcut(event);
      if (shortcut && run(shortcut, event)) {
        event.preventDefault();
        // Not on to xterm.js, which would send the keys to the shell.
        event.stopPropagation();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  return (
    <MotionConfig reducedMotion="user">
      <TooltipProvider delayDuration={500}>
        <ConfirmProvider>
          <div
            className="fixed inset-x-0 flex overflow-hidden bg-bg"
            style={{ top: viewport.top, height: viewport.height }}
            inert={gate !== "open"}
          >
            <section
              aria-label={t.mobileHostsTitle}
              className={`min-w-0 flex-col md:flex md:w-80 md:shrink-0 md:border-r ${open ? "hidden" : "flex w-full"}`}
            >
              {loadError && (
                <Notice className="m-4">
                  {t.loadFailed}: {loadError}
                </Notice>
              )}
              <div className="min-h-0 flex-1">
                {tab === "hosts" && (
                  <MobileHostList
                    hosts={hosts}
                    groups={groups}
                    selectedId={open?.host.id ?? null}
                    onOpen={openHost}
                    onFiles={openFiles}
                    onEdit={setEditing}
                    onAdd={() => setEditing("new")}
                    onSettings={() => setSettings(true)}
                    onChanged={reload}
                  />
                )}
                {tab === "forwards" && <MobileForwards hosts={hosts} />}
                {tab === "sync" && <MobileSync />}
              </div>
              <MobileTabBar tab={tab} onChange={setTab} />
            </section>
            <main className={`relative min-w-0 flex-1 flex-col md:flex ${open ? "flex" : "hidden"}`}>
              {open?.terminal && (
                <MobileTerminal
                  key={`${open.host.id}:${open.attempt}`}
                  host={open.host}
                  onBack={() => setOpen(null)}
                  onFiles={() => setOpen({ ...open, files: true })}
                  onOpenSettings={() => setSettings(true)}
                  controllerRef={controller}
                  keyboardOpen={viewport.keyboardOpen}
                />
              )}
              {open?.files && (
                <div className="absolute inset-0 z-10 bg-bg">
                  <MobileFiles
                    key={`${open.host.id}:files`}
                    host={open.host}
                    onBack={() => setOpen(open.terminal ? { ...open, files: false } : null)}
                    onTerminal={open.terminal ? undefined : () => openHost(open.host)}
                  />
                </div>
              )}
              {!open && (
                <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
                  <img src={mark} alt="" className="size-16" draggable={false} />
                  <h1 className="text-title-2">{t.emptyTitle}</h1>
                </div>
              )}
            </main>
          </div>
          <div inert={gate !== "open"}>
            <AnimatePresence>
              {editing && (
                <MobileHostForm
                  host={editing === "new" ? undefined : editing}
                  hosts={hosts}
                  groups={groups}
                  onCancel={() => setEditing(null)}
                  onDone={() => {
                    setEditing(null);
                    reload();
                  }}
                />
              )}
            </AnimatePresence>
            {settings && vault.status && (
              <MobileSettings
                host={open ? { id: open.host.id, name: open.host.name } : null}
                status={vault.status}
                onStatus={vault.set}
                onBack={() => setSettings(false)}
              />
            )}
            {shortcuts && <ShortcutList onClose={() => setShortcuts(false)} only={MOBILE_SHORTCUTS} />}
          </div>
          {gate === "setup" && vault.status && (
            <MobileVaultSetup status={vault.status} onDone={vault.set} />
          )}
          {gate === "locked" && vault.status && (
            <MobileLockScreen status={vault.status} onUnlocked={vault.set} />
          )}
          <AskpassDialog />
          <HostKeyDialog />
        </ConfirmProvider>
      </TooltipProvider>
    </MotionConfig>
  );
}
