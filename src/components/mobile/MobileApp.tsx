import { AnimatePresence, MotionConfig } from "motion/react";
import { useCallback, useEffect, useState } from "react";
import mark from "@/assets/mark.svg";
import { AskpassDialog } from "@/components/app/AskpassDialog";
import { HostKeyDialog } from "@/components/app/HostKeyDialog";
import { MobileHostForm } from "@/components/mobile/MobileHostForm";
import { MobileHostList } from "@/components/mobile/MobileHostList";
import { MobileTerminal } from "@/components/mobile/MobileTerminal";
import { ConfirmProvider } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { TooltipProvider } from "@/components/ui/tooltip";
import { loadHosts, onHostsChanged, type Host } from "@/lib/backend";
import { t } from "@/lib/i18n";

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

/** The touch UI of the mobile prototype: host list, then a terminal on the
 *  chosen host. Under the `md` breakpoint (834 px, guidelines/layout.md) the
 *  two are screens; from `md` up (a tablet) they sit side by side. */
export function MobileApp() {
  const [hosts, setHosts] = useState<Host[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [open, setOpen] = useState<{ host: Host; attempt: number } | null>(null);
  const [adding, setAdding] = useState(false);
  const height = useVisibleHeight();

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

  return (
    <MotionConfig reducedMotion="user">
      <TooltipProvider delayDuration={500}>
        <ConfirmProvider>
          <div className="flex w-full overflow-hidden bg-bg" style={{ height }}>
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
                />
              </div>
            </section>
            <main className={`min-w-0 flex-1 flex-col md:flex ${open ? "flex" : "hidden"}`}>
              {open ? (
                <MobileTerminal key={`${open.host.id}:${open.attempt}`} host={open.host} onBack={() => setOpen(null)} />
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
          <AskpassDialog />
          <HostKeyDialog />
        </ConfirmProvider>
      </TooltipProvider>
    </MotionConfig>
  );
}
