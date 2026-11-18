import { AnimatePresence, MotionConfig } from "motion/react";
import { useCallback, useEffect, useRef, useState } from "react";
import mark from "@/assets/mark.svg";
import { ShortcutList } from "@/components/app/ShortcutList";
import { AskpassDialog } from "@/components/app/AskpassDialog";
import { HostKeyDialog } from "@/components/app/HostKeyDialog";
import { MobileHostForm } from "@/components/mobile/MobileHostForm";
import { MobileHostList } from "@/components/mobile/MobileHostList";
import { MobileSettings, MOBILE_SHORTCUTS } from "@/components/mobile/MobileSettings";
import { MobileTerminal, type TerminalController } from "@/components/mobile/MobileTerminal";
import { ConfirmProvider } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { TooltipProvider } from "@/components/ui/tooltip";
import { setSessionNotification } from "@/lib/androidBridge";
import { loadHosts, onHostsChanged, type Host } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { useKeyboardViewport } from "@/lib/keyboardViewport";
import { useMobilePrefs } from "@/lib/mobilePrefs";
import { appShortcut, inTerminal, type AppShortcut } from "@/lib/shortcuts";

/** The touch UI of the mobile app: host list, then a terminal on the chosen
 *  host. Under the `md` breakpoint (834 px, guidelines/layout.md) the two are
 *  screens; from `md` up (a tablet) they sit side by side. The app is laid out
 *  to the visual viewport, so the key bar stays above the soft keyboard, and a
 *  hardware keyboard drives the same shortcuts as the desktop where they apply
 *  (guidelines/keyboard.md, K20). */
export function MobileApp() {
  const [hosts, setHosts] = useState<Host[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [open, setOpen] = useState<{ host: Host; attempt: number } | null>(null);
  const [adding, setAdding] = useState(false);
  const [settings, setSettings] = useState(false);
  const [shortcuts, setShortcuts] = useState(false);
  const viewport = useKeyboardViewport();
  const prefs = useMobilePrefs();
  const controller = useRef<TerminalController | null>(null);

  const reload = useCallback(() => {
    loadHosts()
      .then((snapshot) => {
        setHosts(snapshot.hosts);
        setLoadError(null);
      })
      .catch((reason) => setLoadError(String(reason)));
  }, []);
  useEffect(reload, [reload]);
  useEffect(() => onHostsChanged(reload), [reload]);

  const openHost = (host: Host) => setOpen((current) => ({ host, attempt: (current?.attempt ?? 0) + 1 }));

  // The optional Android notification while a terminal is open (opt-in).
  const terminalOpen = open !== null;
  useEffect(() => {
    setSessionNotification(prefs.foregroundService && terminalOpen, t.mobileForegroundTitle, `1 ${t.mobileForegroundText}`);
  }, [prefs.foregroundService, terminalOpen]);
  useEffect(() => () => setSessionNotification(false, "", ""), []);

  // A hardware keyboard (a tablet with a keyboard cover, a phone over Bluetooth).
  const state = useRef({ open, adding, settings, shortcuts });
  state.current = { open, adding, settings, shortcuts };
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
          setAdding(true);
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
                <MobileHostList
                  hosts={hosts}
                  selectedId={open?.host.id ?? null}
                  onOpen={openHost}
                  onAdd={() => setAdding(true)}
                  onSettings={() => setSettings(true)}
                />
              </div>
            </section>
            <main className={`min-w-0 flex-1 flex-col md:flex ${open ? "flex" : "hidden"}`}>
              {open ? (
                <MobileTerminal
                  key={`${open.host.id}:${open.attempt}`}
                  host={open.host}
                  onBack={() => setOpen(null)}
                  onOpenSettings={() => setSettings(true)}
                  controllerRef={controller}
                  keyboardOpen={viewport.keyboardOpen}
                />
              ) : (
                <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
                  <img src={mark} alt="" className="size-16" draggable={false} />
                  <h1 className="text-title-2">{t.emptyTitle}</h1>
                </div>
              )}
            </main>
          </div>
          <AnimatePresence>
            {adding && (
              <MobileHostForm
                onCancel={() => setAdding(false)}
                onDone={() => {
                  setAdding(false);
                  reload();
                }}
              />
            )}
          </AnimatePresence>
          {settings && (
            <MobileSettings host={open ? { id: open.host.id, name: open.host.name } : null} onClose={() => setSettings(false)} />
          )}
          {shortcuts && <ShortcutList onClose={() => setShortcuts(false)} only={MOBILE_SHORTCUTS} />}
          <AskpassDialog />
          <HostKeyDialog />
        </ConfirmProvider>
      </TooltipProvider>
    </MotionConfig>
  );
}
