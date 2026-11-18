import { AnimatePresence, MotionConfig } from "motion/react";
import { useCallback, useEffect, useState } from "react";
import mark from "@/assets/mark.svg";
import { AskpassDialog } from "@/components/app/AskpassDialog";
import { HostKeyDialog } from "@/components/app/HostKeyDialog";
import { MobileFiles } from "@/components/mobile/MobileFiles";
import { MobileForwards } from "@/components/mobile/MobileForwards";
import { MobileHostForm } from "@/components/mobile/MobileHostForm";
import { MobileHostList } from "@/components/mobile/MobileHostList";
import { MobileLockScreen } from "@/components/mobile/MobileLockScreen";
import { MobileSettings } from "@/components/mobile/MobileSettings";
import { MobileSync } from "@/components/mobile/MobileSync";
import { MobileTabBar, type MobileTab } from "@/components/mobile/MobileTabBar";
import { MobileTerminal } from "@/components/mobile/MobileTerminal";
import { MobileVaultSetup } from "@/components/mobile/MobileVaultSetup";
import { ConfirmProvider } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { TooltipProvider } from "@/components/ui/tooltip";
import { loadHosts, onHostsChanged, vaultBackground, vaultForeground, type Group, type Host } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { vaultGate, watchLifecycle } from "@/lib/mobileVault";
import { useVaultStatus } from "@/lib/useVaultStatus";

/** The height of what the person can see. On iOS the soft keyboard covers the
 *  page without resizing it, so the layout follows the visual viewport; on
 *  Android both agree. */
function useVisibleHeight(): number {
  const read = () => window.visualViewport?.height ?? window.innerHeight;
  const [height, setHeight] = useState(read);
  useEffect(() => {
    const viewport = window.visualViewport;
    const update = () => {
      setHeight(read());
      // iOS scrolls the page to keep the focused field in view; the layout
      // already fits, so put it back.
      if (window.scrollY !== 0) window.scrollTo(0, 0);
    };
    viewport?.addEventListener("resize", update);
    viewport?.addEventListener("scroll", update);
    window.addEventListener("resize", update);
    return () => {
      viewport?.removeEventListener("resize", update);
      viewport?.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
    };
  }, []);
  return height;
}

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
 *  host are screens; from `md` up (a tablet) they sit side by side. */
export function MobileApp() {
  const [hosts, setHosts] = useState<Host[]>([]);
  const [groups, setGroups] = useState<Group[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [tab, setTab] = useState<MobileTab>("hosts");
  const [open, setOpen] = useState<Open | null>(null);
  const [editing, setEditing] = useState<Host | "new" | null>(null);
  const [settings, setSettings] = useState(false);
  const vault = useVaultStatus();
  const gate = vaultGate(vault.status);
  const height = useVisibleHeight();

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

  const openTerminal = (host: Host) =>
    setOpen((current) => ({
      host,
      attempt: (current?.attempt ?? 0) + 1,
      terminal: true,
      files: false,
    }));
  const openFiles = (host: Host) =>
    setOpen((current) =>
      current?.terminal && current.host.id === host.id
        ? { ...current, files: true }
        : { host, attempt: (current?.attempt ?? 0) + 1, terminal: false, files: true },
    );

  return (
    <MotionConfig reducedMotion="user">
      <TooltipProvider delayDuration={500}>
        <ConfirmProvider>
          <div className="flex w-full overflow-hidden bg-bg" style={{ height }} inert={gate !== "open"}>
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
                    onOpen={openTerminal}
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
                <div className="flex h-full min-h-0 flex-col">
                  <MobileTerminal
                    key={`${open.host.id}:${open.attempt}`}
                    host={open.host}
                    onBack={() => setOpen(null)}
                    onFiles={() => setOpen({ ...open, files: true })}
                  />
                </div>
              )}
              {open?.files && (
                <div className="absolute inset-0 z-10 bg-bg">
                  <MobileFiles
                    key={`${open.host.id}:files`}
                    host={open.host}
                    onBack={() => setOpen(open.terminal ? { ...open, files: false } : null)}
                    onTerminal={open.terminal ? undefined : () => openTerminal(open.host)}
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
              <MobileSettings status={vault.status} onStatus={vault.set} onBack={() => setSettings(false)} />
            )}
          </div>
          {gate === "setup" && vault.status && <MobileVaultSetup status={vault.status} onDone={vault.set} />}
          {gate === "locked" && vault.status && <MobileLockScreen status={vault.status} onUnlocked={vault.set} />}
          <AskpassDialog />
          <HostKeyDialog />
        </ConfirmProvider>
      </TooltipProvider>
    </MotionConfig>
  );
}
