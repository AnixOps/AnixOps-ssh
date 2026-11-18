import { FolderOpen, Pencil, Plus, Search, Server, SquareTerminal, Star, StarOff, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Input } from "@/components/ui/input";
import { Menu } from "@/components/ui/menu";
import { deleteHost, hostColor, setFavorite, type Group, type Host } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { groupName, listHosts } from "@/lib/mobileHosts";
import { cn } from "@/lib/utils";

/** The host list of the phone: a title bar, a search field (name, address, group
 *  or `#tag`) and rows of 44 px or more with the two values that matter (name,
 *  then user@host) on two lines (density.md N5). Tapping a row opens a
 *  terminal on it; the `⋯` button of a row has the rest. */
export function MobileHostList({
  hosts,
  groups,
  selectedId,
  onOpen,
  onFiles,
  onEdit,
  onAdd,
  onChanged,
}: {
  hosts: Host[];
  groups: Group[];
  selectedId: string | null;
  onOpen: (host: Host) => void;
  onFiles: (host: Host) => void;
  onEdit: (host: Host) => void;
  onAdd: () => void;
  onChanged: () => void;
}) {
  const [query, setQuery] = useState("");
  const [failure, setFailure] = useState<string | null>(null);
  const confirm = useConfirm();
  const shown = useMemo(() => listHosts(hosts, groups, query), [hosts, groups, query]);

  const run = (action: Promise<void>) =>
    action.then(onChanged).catch((reason) => setFailure(String(reason)));

  const remove = async (host: Host) => {
    const ok = await confirm({
      title: t.hostDeleteTitle.replace("{name}", host.name),
      body: t.hostDeleteBody,
      verb: t.delete,
    });
    if (ok) void run(deleteHost(host.id));
  };

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
              placeholder={t.mobileSearchHosts}
              aria-label={t.searchHosts}
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              className="h-11 pl-9"
            />
          </div>
        </div>
      </header>

      {failure && (
        <p role="alert" className="px-safe py-2 text-callout text-danger">
          {failure}
        </p>
      )}

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
          {shown.map((host) => {
            const group = groupName(host, groups);
            const detail = [group, ...host.tags.map((tag) => `#${tag}`)].filter(Boolean).join(" · ");
            const editable = host.source === "saved";
            return (
              <li key={host.id} className="flex items-center border-b pr-2">
                <button
                  type="button"
                  onClick={() => onOpen(host)}
                  aria-current={host.id === selectedId ? "true" : undefined}
                  className={cn(
                    "px-safe flex min-h-14 min-w-0 flex-1 items-center gap-3 py-2 text-left transition-colors duration-micro ease-standard active:bg-fill-1 aria-[current=true]:bg-fill-1",
                  )}
                >
                  <span
                    aria-hidden
                    className="size-2.5 shrink-0 rounded-pill bg-label-3"
                    style={host.color ? { backgroundColor: hostColor(host.color) } : undefined}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="flex items-center gap-1.5">
                      <span className="truncate text-body text-label-1">{host.name}</span>
                      {host.favorite && (
                        <Star aria-label={t.favorite} className="size-3.5 shrink-0 text-label-3" strokeWidth={1.5} />
                      )}
                    </span>
                    <span className="block truncate text-callout text-label-2">{host.target}</span>
                    {detail && <span className="block truncate text-caption text-label-3">{detail}</span>}
                  </span>
                </button>
                <Menu
                  touch
                  label={`${t.more}: ${host.name}`}
                  items={[
                    { key: "open", label: t.mobileOpenTerminal, icon: <SquareTerminal />, onSelect: () => onOpen(host) },
                    { key: "files", label: t.files, icon: <FolderOpen />, onSelect: () => onFiles(host) },
                    ...(editable
                      ? [
                          { key: "edit", label: t.edit, icon: <Pencil />, onSelect: () => onEdit(host) },
                          {
                            key: "favorite",
                            label: host.favorite ? t.removeFavorite : t.addFavorite,
                            icon: host.favorite ? <StarOff /> : <Star />,
                            onSelect: () => void run(setFavorite(host.id, !host.favorite)),
                          },
                          "separator" as const,
                          {
                            key: "delete",
                            label: t.delete,
                            icon: <Trash2 />,
                            danger: true,
                            onSelect: () => void remove(host),
                          },
                        ]
                      : []),
                  ]}
                />
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
