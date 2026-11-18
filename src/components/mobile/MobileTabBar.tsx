import { ArrowRightLeft, RefreshCw, Server, type LucideIcon } from "lucide-react";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";

export type MobileTab = "hosts" | "forwards" | "sync";

const TABS: Array<{ id: MobileTab; icon: LucideIcon; label: () => string }> = [
  { id: "hosts", icon: Server, label: () => t.mobileHostsTitle },
  { id: "forwards", icon: ArrowRightLeft, label: () => t.mobileTabForwards },
  { id: "sync", icon: RefreshCw, label: () => t.syncTitle },
];

/** The app-level navigation of the phone: a bar at the bottom with three
 *  destinations (platforms.md), each an icon over a word, 56 px tall. */
export function MobileTabBar({ tab, onChange }: { tab: MobileTab; onChange: (tab: MobileTab) => void }) {
  return (
    <nav aria-label={t.mobileTabs} className="px-safe pb-safe material-bar flex shrink-0 border-t">
      {TABS.map(({ id, icon: Icon, label }) => (
        <button
          key={id}
          type="button"
          aria-current={tab === id ? "page" : undefined}
          onClick={() => onChange(id)}
          className={cn(
            "flex min-h-14 flex-1 flex-col items-center justify-center gap-0.5 text-caption transition-colors duration-micro ease-standard",
            tab === id ? "text-(--accent)" : "text-label-2",
          )}
        >
          <Icon className="size-5" strokeWidth={1.5} />
          {label()}
        </button>
      ))}
    </nav>
  );
}
