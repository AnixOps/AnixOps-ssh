import { ArrowDown, ArrowUp, Copy, OctagonX, RotateCw, Search, Skull, X } from "lucide-react";
import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Menu } from "@/components/ui/menu";
import { Overlay } from "@/components/ui/overlay";
import { Tooltip } from "@/components/ui/tooltip";
import { copyText, monitorFailure, monitorSignal, type MonitorSignal } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { formatBytes, sortProcesses, type MonitorView, type Process, type ProcessSort } from "@/lib/monitor";
import { cn } from "@/lib/utils";

const columns: { key: ProcessSort; label: () => string; className: string; numeric?: boolean }[] = [
  { key: "pid", label: () => t.processPid, className: "w-20", numeric: true },
  { key: "user", label: () => t.processUser, className: "w-28" },
  { key: "rss", label: () => t.processMemory, className: "w-24", numeric: true },
  { key: "cpu", label: () => t.processCpu, className: "w-16", numeric: true },
  { key: "name", label: () => t.processCommand, className: "" },
];

/** Text a process matches the filter on. */
function haystack(proc: Process): string {
  return `${proc.pid} ${proc.user ?? ""} ${proc.name} ${proc.args ?? ""} ${proc.exe ?? ""}`.toLowerCase();
}

/** Every process on the server, sortable by any column, with filtering and
 *  ending a process (TERM, or KILL after a confirmation). Updates with the
 *  monitor's samples while open. */
export function ProcessDialog({
  hostId,
  hostName,
  current,
  onRefresh,
  onClose,
}: {
  hostId: string;
  hostName: string;
  current: MonitorView;
  onRefresh: () => void;
  onClose: () => void;
}) {
  const confirm = useConfirm();
  const [sort, setSort] = useState<ProcessSort>("cpu");
  const [descending, setDescending] = useState(true);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState<string | null>(null);

  const rows = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const shown = needle ? current.processes.filter((proc) => haystack(proc).includes(needle)) : current.processes;
    return sortProcesses(shown, sort, descending);
  }, [current.processes, filter, sort, descending]);

  const sortBy = (key: ProcessSort) => {
    if (key === sort) setDescending((value) => !value);
    else {
      setSort(key);
      // Numbers read biggest first; names alphabetically.
      setDescending(key === "cpu" || key === "rss");
    }
  };

  const send = async (proc: Process, signal: MonitorSignal) => {
    const vars = (text: string) => text.replace("{name}", proc.name).replace("{pid}", String(proc.pid));
    const ok = await confirm({
      title: vars(signal === "KILL" ? t.processKillTitle : t.processEndTitle),
      body: signal === "KILL" ? t.processKillBody : t.processEndBody,
      verb: signal === "KILL" ? t.processKill : t.processEnd,
    });
    if (!ok) return;
    setError(null);
    try {
      await monitorSignal(hostId, proc.pid, signal);
      onRefresh();
    } catch (reason) {
      const failure = monitorFailure(reason);
      setError(`${t.processSignalFailed}: ${failure.message}`);
    }
  };

  return (
    <Overlay label={t.processesTitle.replace("{host}", hostName)} onDismiss={onClose} className="flex h-full max-w-6xl flex-col">
      <header className="flex h-14 shrink-0 items-center gap-2 border-b px-4">
        <h2 className="mr-2 truncate text-title-3">{t.processesTitle.replace("{host}", hostName)}</h2>
        <span className="mr-auto shrink-0 text-callout text-label-2">{t.processCount.replace("{n}", String(rows.length))}</span>
        <div className="relative w-64">
          <Search aria-hidden className="pointer-events-none absolute top-2.5 left-2.5 size-4 text-label-3" strokeWidth={1.5} />
          <Input
            autoFocus
            aria-label={t.processFilter}
            placeholder={t.processFilter}
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
            className="pl-8"
          />
        </div>
        <Tooltip label={t.refresh}>
          <Button variant="quiet" size="icon" aria-label={t.refresh} onClick={onRefresh}>
            <RotateCw />
          </Button>
        </Tooltip>
        <Tooltip label={t.close}>
          <Button variant="quiet" size="icon" aria-label={t.close} onClick={onClose}>
            <X />
          </Button>
        </Tooltip>
      </header>
      {error && <Notice className="mx-4 mt-3">{error}</Notice>}
      <div className="min-h-0 flex-1 overflow-auto">
        <table className="w-full table-fixed text-callout">
          <thead className="sticky top-0 z-sticky bg-bg-elevated">
            <tr className="border-b text-left">
              {columns.map((column) => {
                const active = column.key === sort;
                return (
                  <th
                    key={column.key}
                    scope="col"
                    aria-sort={active ? (descending ? "descending" : "ascending") : "none"}
                    className={cn("h-9 p-0 font-normal", column.className)}
                  >
                    <button
                      type="button"
                      onClick={() => sortBy(column.key)}
                      title={t.processSort}
                      className={cn(
                        "flex h-9 w-full items-center gap-1 px-3 text-label-2 hover:text-label-1",
                        column.numeric && "justify-end",
                        active && "text-label-1",
                      )}
                    >
                      {column.label()}
                      {active && (descending ? <ArrowDown className="size-4" strokeWidth={1.5} /> : <ArrowUp className="size-4" strokeWidth={1.5} />)}
                    </button>
                  </th>
                );
              })}
              <th scope="col" className="w-64 px-3 font-normal text-label-2">
                {t.processLocation}
              </th>
              <th scope="col" className="w-12">
                <span className="sr-only">{t.more}</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((proc) => (
              <tr key={proc.pid} className="group/row h-8 border-b border-separator hover:bg-fill-1">
                <td className="px-3 text-right font-mono tabular-nums text-label-2">{proc.pid}</td>
                <td className="truncate px-3 text-label-2" title={proc.user}>
                  {proc.user ?? ""}
                </td>
                <td className="px-3 text-right font-mono tabular-nums text-label-1">{formatBytes(proc.rss)}</td>
                <td className="px-3 text-right font-mono tabular-nums text-label-1">{proc.cpu.toFixed(1)}</td>
                <td className="selectable truncate px-3 text-label-1" title={proc.args || proc.name}>
                  {proc.name}
                  {proc.args && proc.args !== proc.name && <span className="ml-2 text-label-2">{proc.args}</span>}
                </td>
                <td className="selectable truncate px-3 font-mono text-label-2" title={proc.exe}>
                  {proc.exe ?? ""}
                </td>
                <td className="pr-2 text-right">
                  <Menu
                    items={[
                      { key: "pid", label: t.processCopyPid, icon: <Copy />, onSelect: () => void copyText(String(proc.pid)) },
                      {
                        key: "args",
                        label: t.processCopyCommand,
                        icon: <Copy />,
                        onSelect: () => void copyText(proc.args || proc.name),
                      },
                      "separator",
                      { key: "end", label: t.processEnd, icon: <OctagonX />, danger: true, onSelect: () => void send(proc, "TERM") },
                      { key: "kill", label: t.processKill, icon: <Skull />, danger: true, onSelect: () => void send(proc, "KILL") },
                    ]}
                  />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Overlay>
  );
}
