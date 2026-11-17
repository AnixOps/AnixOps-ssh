import { Cable, ChevronRight, Copy, Download, Pencil, Plus, Search, Server, Star, Trash2 } from "lucide-react";
import { useMemo, useState, type KeyboardEvent, type Ref } from "react";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Input } from "@/components/ui/input";
import { Menu, type MenuEntry } from "@/components/ui/menu";
import { Tooltip } from "@/components/ui/tooltip";
import { hostColor, isTauri, type Group, type Host } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { shortcutHint } from "@/lib/shortcutTable";
import { cn } from "@/lib/utils";

interface Section {
  id: string;
  title: string;
  hosts: Host[];
}

function buildSections(hosts: Host[], groups: Group[]): Section[] {
  const sections: Section[] = [];
  const favorites = hosts.filter((host) => host.favorite);
  if (favorites.length > 0) sections.push({ id: "favorites", title: t.favorites, hosts: favorites });
  for (const group of groups) {
    const members = hosts.filter((host) => host.groupId === group.id);
    if (members.length > 0) sections.push({ id: group.id, title: group.name, hosts: members });
  }
  const known = new Set(groups.map((group) => group.id));
  const ungrouped = hosts.filter(
    (host) => host.source === "saved" && (host.groupId === null || !known.has(host.groupId)),
  );
  if (ungrouped.length > 0) sections.push({ id: "ungrouped", title: t.ungrouped, hosts: ungrouped });
  const sshConfig = hosts.filter((host) => host.source === "ssh_config");
  if (sshConfig.length > 0) sections.push({ id: "ssh_config", title: t.fromSshConfig, hosts: sshConfig });
  return sections;
}

export function HostList({
  hosts,
  groups,
  selectedId,
  onOpen,
  onEdit,
  onToggleFavorite,
  onDelete,
  onImport,
  onNewSerial,
  searchRef,
}: {
  hosts: Host[];
  groups: Group[];
  selectedId: string | null;
  onOpen: (host: Host) => void;
  /** null creates a new host. */
  onEdit: (host: Host | null) => void;
  onToggleFavorite: (host: Host) => void;
  onDelete: (host: Host) => void;
  onImport: () => void;
  onNewSerial: () => void;
  searchRef: Ref<HTMLInputElement>;
}) {
  const [query, setQuery] = useState("");
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const normalized = query.trim().toLowerCase();
  const matches = useMemo(
    () =>
      hosts.filter(
        (host) =>
          host.name.toLowerCase().includes(normalized) ||
          host.target.toLowerCase().includes(normalized) ||
          host.tags.some((tag) => tag.toLowerCase().includes(normalized)),
      ),
    [hosts, normalized],
  );
  const sections = useMemo(() => buildSections(hosts, groups), [hosts, groups]);

  const toggle = (id: string) =>
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  // The list is one Tab stop; arrows move between rows (K7). The selected host, or
  // the first row, is the stop.
  const visible = normalized
    ? matches
    : sections.filter((section) => !collapsed.has(section.id)).flatMap((section) => section.hosts);
  const stopId = visible.find((host) => host.id === selectedId)?.id ?? visible[0]?.id;
  // Hosts repeat under "收藏"; only the first row for the stop's host is tabbable.
  let stopTaken = false;
  const isStop = (host: Host) => {
    if (host.id !== stopId || stopTaken) return false;
    stopTaken = true;
    return true;
  };
  const moveRow = (event: KeyboardEvent<HTMLDivElement>) => {
    const rows = [...event.currentTarget.querySelectorAll<HTMLElement>("[data-host-row]")];
    const index = rows.indexOf(document.activeElement as HTMLElement);
    if (index < 0) return;
    const to =
      event.key === "ArrowDown"
        ? index + 1
        : event.key === "ArrowUp"
          ? index - 1
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? rows.length - 1
              : null;
    if (to === null) return;
    event.preventDefault();
    rows[Math.max(0, Math.min(rows.length - 1, to))]?.focus();
  };

  const row = (host: Host) => (
    <HostRow
      key={host.id}
      host={host}
      tabStop={isStop(host)}
      selected={host.id === selectedId}
      onOpen={onOpen}
      onEdit={onEdit}
      onToggleFavorite={onToggleFavorite}
      onDelete={onDelete}
    />
  );

  return (
    <aside className="material-sidebar flex w-72 shrink-0 flex-col border-r">
      <div className="flex items-center gap-2 px-3 pt-3 pb-2">
        <div className="relative min-w-0 flex-1">
          <Search
            className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-label-3"
            strokeWidth={1.5}
          />
          <Input
            ref={searchRef}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape" && query) {
                event.stopPropagation();
                setQuery("");
              } else if (event.key === "ArrowDown") {
                event.preventDefault();
                document.querySelector<HTMLElement>("[data-host-row]")?.focus();
              }
            }}
            placeholder={t.searchHosts}
            aria-label={t.searchHosts}
            className="pl-9"
          />
        </div>
        <Menu
          label={t.addHost}
          icon={<Plus />}
          variant="secondary"
          size="icon"
          items={[
            { key: "new", label: t.newHost, icon: <Plus />, shortcut: shortcutHint("new-host"), onSelect: () => onEdit(null) },
            { key: "new-serial", label: t.newSerialHost, icon: <Cable />, onSelect: onNewSerial },
            { key: "import", label: t.importHosts, icon: <Download />, shortcut: shortcutHint("import"), onSelect: onImport },
          ]}
        />
      </div>

      <div className="flex-1 overflow-y-auto px-2 pb-3" onKeyDown={moveRow}>
        {hosts.length === 0 ? (
          <div className="px-3 pt-10 text-center">
            <Server className="mx-auto mb-3 size-12 text-label-3" strokeWidth={1.5} />
            <p className="text-callout font-medium text-label-1">{t.noHosts}</p>
            <p className="mt-1 text-callout text-label-2">{t.noHostsHint}</p>
            <div className="mt-4 flex items-center justify-center gap-2">
              <Button variant="primary" size="sm" onClick={() => onEdit(null)}>
                <Plus />
                {t.newHost}
              </Button>
              <Button variant="secondary" size="sm" onClick={onImport}>
                <Download />
                {t.importHosts}
              </Button>
            </div>
          </div>
        ) : normalized ? (
          matches.length === 0 ? (
            <p className="px-3 pt-6 text-center text-callout text-label-2">{t.noMatches}</p>
          ) : (
            <ul className="mt-1 space-y-0.5">{matches.map(row)}</ul>
          )
        ) : (
          sections.map((section) => {
            const isCollapsed = collapsed.has(section.id);
            return (
              <section key={section.id} className="mt-2">
                <button
                  type="button"
                  onClick={() => toggle(section.id)}
                  aria-expanded={!isCollapsed}
                  className="flex h-7 w-full items-center gap-1 rounded-xs px-2 text-callout text-label-2 hover:text-label-1"
                >
                  <ChevronRight
                    className={cn("size-4 transition-transform duration-toggle ease-standard", !isCollapsed && "rotate-90")}
                    strokeWidth={1.5}
                  />
                  <span className="truncate">{section.title}</span>
                  <span className="ml-auto text-label-3 tabular-nums">{section.hosts.length}</span>
                </button>
                {!isCollapsed && <ul className="space-y-0.5">{section.hosts.map(row)}</ul>}
              </section>
            );
          })
        )}
      </div>

      {!isTauri && <p className="border-t px-4 py-2 text-callout text-label-3">{t.previewData}</p>}
    </aside>
  );
}

function HostRow({
  host,
  tabStop,
  selected,
  onOpen,
  onEdit,
  onToggleFavorite,
  onDelete,
}: {
  host: Host;
  tabStop: boolean;
  selected: boolean;
  onOpen: (host: Host) => void;
  onEdit: (host: Host) => void;
  onToggleFavorite: (host: Host) => void;
  onDelete: (host: Host) => void;
}) {
  const saved = host.source === "saved";
  const confirm = useConfirm();
  const remove = async () => {
    const title = t.hostDeleteTitle.replace("{name}", host.name);
    if (await confirm({ title, body: t.hostDeleteBody, verb: t.delete })) onDelete(host);
  };
  const menu: MenuEntry[] = [
    {
      key: "edit",
      label: saved ? t.editHost : t.saveToHosts,
      icon: <Pencil />,
      shortcut: saved ? shortcutHint("rename") : undefined,
      onSelect: () => onEdit(host),
    },
    ...(saved
      ? [
          {
            key: "favorite",
            label: host.favorite ? t.removeFavorite : t.addFavorite,
            icon: <Star />,
            onSelect: () => onToggleFavorite(host),
          },
        ]
      : []),
    { key: "copy", label: t.copyAddress, icon: <Copy />, onSelect: () => void navigator.clipboard?.writeText(host.target) },
    ...(saved
      ? [
          "separator" as const,
          { key: "delete", label: t.delete, icon: <Trash2 />, danger: true, shortcut: shortcutHint("remove"), onSelect: () => void remove() },
        ]
      : []),
  ];
  return (
    <li
      className="group relative"
      onKeyDown={(event) => {
        // Keys on a focused row (keyboard.md, lists): F2 edits, Delete removes after a
        // confirmation, the menu key opens the row menu.
        if (event.key === "F2" && saved) onEdit(host);
        else if (event.key === "Delete" && saved) void remove();
        else if ((event.key === "F10" && event.shiftKey) || event.key === "ContextMenu")
          event.currentTarget.querySelector<HTMLElement>("[data-row-menu]")?.click();
        else return;
        event.preventDefault();
      }}
    >
      <button
        type="button"
        data-host-row
        tabIndex={tabStop ? 0 : -1}
        onClick={() => onOpen(host)}
        aria-current={selected ? "true" : undefined}
        className={cn(
          "relative flex w-full items-center gap-3 rounded-sm py-1.5 pr-16 pl-2 text-left",
          selected ? "bg-accent-soft" : "hover:bg-fill-1",
        )}
      >
        <HostTile host={host} selected={selected} />
        <span className="relative min-w-0 flex-1">
          <span className="block truncate text-callout font-medium text-label-1" title={host.name}>
            {host.name}
          </span>
          <span className="block truncate font-mono text-callout text-label-2" title={host.target}>
            {host.target}
          </span>
        </span>
      </button>
      {/* S3: one icon action and the "more" menu, in space that is always reserved so
          nothing moves under the pointer (M13). */}
      <span
        className={cn(
          "absolute top-1/2 right-1.5 flex -translate-y-1/2 items-center transition-opacity duration-micro",
          !host.favorite && "opacity-0 group-focus-within:opacity-100 group-hover:opacity-100",
        )}
      >
        {saved && (
          <Tooltip label={host.favorite ? t.removeFavorite : t.addFavorite}>
            <Button
              variant="quiet"
              size="icon-sm"
              tabIndex={-1}
              aria-label={host.favorite ? t.removeFavorite : t.addFavorite}
              aria-pressed={host.favorite}
              onClick={() => onToggleFavorite(host)}
            >
              <Star className={cn(host.favorite && "fill-current")} />
            </Button>
          </Tooltip>
        )}
        <Menu label={t.more} items={menu} buttonProps={{ tabIndex: -1, "data-row-menu": "" }} />
      </span>
    </li>
  );
}

export function HostTile({ host, selected = false, large = false }: { host: Host; selected?: boolean; large?: boolean }) {
  return (
    <span
      className={cn(
        "relative flex shrink-0 items-center justify-center rounded-sm transition-colors duration-toggle ease-standard",
        large ? "size-12" : "size-8",
        selected ? "bg-bg-elevated text-(--accent) shadow-1" : "bg-fill-2 text-label-2",
      )}
    >
      {host.fields.serial ? (
        <Cable className={large ? "size-6" : "size-4"} strokeWidth={1.5} />
      ) : (
        <Server className={large ? "size-6" : "size-4"} strokeWidth={1.5} />
      )}
      {host.color && (
        <span
          aria-hidden
          className="absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full ring-2 ring-(--bg-grouped)"
          style={{ backgroundColor: hostColor(host.color) }}
        />
      )}
    </span>
  );
}
