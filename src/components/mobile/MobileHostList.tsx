import { ChevronRight, Plus, Search, Server } from "lucide-react";
import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { hostColor, type Host } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** The host list of the phone: a title bar, a search field and rows of 44 px or
 *  more with the two values that matter (name, then user@host) on two lines
 *  (density.md N5). Tapping a row opens a terminal on it. */
export function MobileHostList({
  hosts,
  selectedId,
  onOpen,
  onAdd,
}: {
  hosts: Host[];
  selectedId: string | null;
  onOpen: (host: Host) => void;
  onAdd: () => void;
}) {
  const [query, setQuery] = useState("");
  const normalized = query.trim().toLowerCase();
  const shown = useMemo(
    () =>
      hosts
        .filter(
          (host) =>
            host.fields.serial == null &&
            (host.name.toLowerCase().includes(normalized) || host.target.toLowerCase().includes(normalized)),
        )
        .sort((a, b) => Number(b.favorite) - Number(a.favorite) || a.name.localeCompare(b.name)),
    [hosts, normalized],
  );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="material-bar pt-safe shrink-0 border-b">
        <div className="px-safe flex h-14 items-center justify-between gap-2">
          <h1 className="truncate text-title-3">{t.mobileHostsTitle}</h1>
          <Button variant="quiet" size="icon-lg" aria-label={t.mobileAddHost} onClick={onAdd}>
            <Plus />
          </Button>
        </div>
        <div className="px-safe pb-3">
          <div className="relative">
            <Search
              className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-label-3"
              strokeWidth={1.5}
            />
            <Input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t.searchHosts}
              aria-label={t.searchHosts}
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              className="h-11 pl-9"
            />
          </div>
        </div>
      </header>

      {shown.length === 0 ? (
        <div className="px-safe flex flex-1 flex-col items-center justify-center gap-2 text-center">
          <Server className="size-8 text-label-3" strokeWidth={1.5} />
          <p className="text-body font-medium">{hosts.length === 0 ? t.noHosts : t.noMatches}</p>
          {hosts.length === 0 && (
            <>
              <p className="max-w-xs text-callout text-label-2">{t.mobileNoHostsHint}</p>
              <Button variant="primary" size="lg" className="mt-4" onClick={onAdd}>
                <Plus />
                {t.mobileAddHost}
              </Button>
            </>
          )}
        </div>
      ) : (
        <ul className="min-h-0 flex-1 overflow-y-auto pb-safe">
          {shown.map((host) => (
            <li key={host.id} className="border-b">
              <button
                type="button"
                onClick={() => onOpen(host)}
                aria-current={host.id === selectedId ? "true" : undefined}
                className={cn(
                  "px-safe flex min-h-14 w-full items-center gap-3 py-2 text-left transition-colors duration-micro ease-standard active:bg-fill-1 aria-[current=true]:bg-fill-1",
                )}
              >
                <span
                  aria-hidden
                  className="size-2.5 shrink-0 rounded-pill bg-label-3"
                  style={host.color ? { backgroundColor: hostColor(host.color) } : undefined}
                />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-body text-label-1">{host.name}</span>
                  <span className="block truncate text-callout text-label-2">{host.target}</span>
                </span>
                <ChevronRight className="size-5 shrink-0 text-label-3" strokeWidth={1.5} />
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
