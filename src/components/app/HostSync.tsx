import { CircleAlert, History, RotateCcw, Users } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import { Tooltip } from "@/components/ui/tooltip";
import { t } from "@/lib/i18n";
import {
  hostHistory,
  hostVaults,
  listVaults,
  moveHostToVault,
  restoreHostHistory,
  type HostHistoryEntry,
  type VaultInfo,
} from "@/lib/syncApi";
import { agoText, syncErrorText, useSyncStatus } from "@/lib/syncText";
import { historyValueText } from "@/lib/syncUi";

/** Earlier values of a saved host's fields, newest first, each with a Restore
 *  that is an ordinary edit (so it syncs like one). Hidden while there is no
 *  background service to ask. */
export function HostHistory({ hostId, onRestored }: { hostId: string; onRestored: () => void }) {
  const [entries, setEntries] = useState<HostHistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hidden, setHidden] = useState(false);

  const reload = useCallback(() => {
    hostHistory(hostId)
      .then(setEntries)
      .catch((reason: unknown) => {
        if (reason && typeof reason === "object" && "code" in reason && reason.code === "unavailable") setHidden(true);
        else setError(syncErrorText(reason));
      });
  }, [hostId]);
  useEffect(reload, [reload]);

  const restore = async (entry: HostHistoryEntry) => {
    try {
      await restoreHostHistory(hostId, entry.seq);
      onRestored();
    } catch (reason) {
      setError(syncErrorText(reason));
    }
  };

  if (hidden || entries === null) return null;
  const label = (field: string) => t.historyFields[field] ?? field;

  return (
    <section aria-labelledby="host-editor-history" className="space-y-2 rounded-md bg-fill-1 p-3">
      <h3 id="host-editor-history" className="flex items-center gap-1.5 text-callout text-label-2">
        <History className="size-4" strokeWidth={1.5} />
        {t.historyTitle}
      </h3>
      {entries.length === 0 ? (
        <p className="text-callout text-label-3">{t.historyEmpty}</p>
      ) : (
        <ul className="max-h-40 divide-y divide-separator overflow-y-auto">
          {entries.map((entry) => {
            const value = historyValueText(entry.field, entry.value);
            const secret = entry.field === "notes_secret";
            const when = new Date(entry.changed_at);
            return (
              <li key={entry.seq} className="flex items-center gap-2 py-1.5">
                <div className="min-w-0 flex-1">
                  <p className="text-callout text-label-1">{label(entry.field)}</p>
                  <p className="selectable truncate font-mono text-callout text-label-2">
                    {secret ? "••••" : value || t.historyEmptyValue}
                  </p>
                  <p className="text-callout text-label-3">
                    {Number.isNaN(when.getTime()) ? "" : `${agoText(when)} · `}
                    {t.historyDevice} {entry.device}
                  </p>
                </div>
                <Tooltip label={`${t.historyRestore}: ${label(entry.field)}`}>
                  <Button
                    type="button"
                    variant="quiet"
                    size="icon-sm"
                    aria-label={`${t.historyRestore}: ${label(entry.field)}`}
                    onClick={() => void restore(entry)}
                  >
                    <RotateCcw />
                  </Button>
                </Tooltip>
              </li>
            );
          })}
        </ul>
      )}
      {error && (
        <p role="alert" className="flex items-start gap-2 text-callout break-words text-danger">
          <CircleAlert className="mt-px size-3.5 shrink-0" strokeWidth={1.5} />
          <span className="selectable min-w-0">{error}</span>
        </p>
      )}
    </section>
  );
}

/** Moves a saved host into a team vault you can edit. Only offered when
 *  signed in and such a vault exists; the host gets a new id, so the editor
 *  closes afterwards. */
export function HostVault({ hostId, onMoved }: { hostId: string; onMoved: () => void }) {
  const { status } = useSyncStatus();
  const [vaults, setVaults] = useState<VaultInfo[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [target, setTarget] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const signedIn = status?.signed_in ?? false;

  useEffect(() => {
    if (!signedIn) return;
    Promise.all([listVaults(), hostVaults()])
      .then(([list, map]) => {
        const writable = list.filter((v) => v.kind === "team" && v.role !== "viewer");
        setVaults(writable);
        setCurrent(map[hostId] ?? null);
        setTarget(writable[0]?.id ?? "");
      })
      .catch(() => setVaults([]));
  }, [signedIn, hostId]);

  if (!signedIn || vaults.length === 0) return null;
  const where = vaults.find((v) => v.id === current)?.name ?? t.teamPersonal;
  const options = vaults.filter((v) => v.id !== current);

  const move = async () => {
    setBusy(true);
    setError(null);
    try {
      await moveHostToVault(hostId, target);
      onMoved();
    } catch (reason) {
      setError(syncErrorText(reason));
      setBusy(false);
    }
  };

  return (
    <section aria-labelledby="host-editor-vault" className="space-y-2 rounded-md bg-fill-1 p-3">
      <h3 id="host-editor-vault" className="flex items-center gap-1.5 text-callout text-label-2">
        <Users className="size-4" strokeWidth={1.5} />
        {t.hostVault}: {where}
      </h3>
      {options.length > 0 && (
        <div className="flex items-center gap-2">
          <Select
            aria-label={t.hostVaultMove}
            value={target}
            onChange={(event) => setTarget(event.target.value)}
            className="h-8 flex-1"
          >
            {options.map((vault) => (
              <option key={vault.id} value={vault.id}>
                {vault.name}
              </option>
            ))}
          </Select>
          <Button type="button" variant="secondary" size="sm" className="h-8" disabled={busy || !target} onClick={() => void move()}>
            {t.hostVaultMove}
          </Button>
        </div>
      )}
      <p className="text-callout text-label-3">{t.hostVaultHint}</p>
      {error && (
        <p role="alert" className="flex items-start gap-2 text-callout break-words text-danger">
          <CircleAlert className="mt-px size-3.5 shrink-0" strokeWidth={1.5} />
          <span className="selectable min-w-0">{error}</span>
        </p>
      )}
    </section>
  );
}
