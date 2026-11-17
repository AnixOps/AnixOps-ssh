import { CircleAlert, Download, LoaderCircle, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { importKnownHosts, listKnownHosts, removeKnownHost, type KnownHost } from "@/lib/backend";
import { t } from "@/lib/i18n";

/** The list is scrolled, but a large ~/.ssh/known_hosts should not render thousands of rows. */
const VISIBLE = 200;

/** Stored host keys with a remove button each, and an import of ~/.ssh/known_hosts. */
export function KnownHosts() {
  const [entries, setEntries] = useState<KnownHost[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);

  const reload = useCallback(() => {
    listKnownHosts()
      .then(setEntries)
      .catch((reason) => setMessage({ text: String(reason), error: true }));
  }, []);
  useEffect(reload, [reload]);

  const doImport = async () => {
    setBusy(true);
    setMessage(null);
    try {
      const report = await importKnownHosts();
      const parts = [`${t.imported}: ${report.imported}`];
      if (report.duplicates) parts.push(`${t.knownHostsDuplicates}: ${report.duplicates}`);
      if (report.skipped.length) parts.push(`${t.skipped}: ${report.skipped.length}`);
      setMessage({ text: parts.join(" · "), error: false });
    } catch (reason) {
      setMessage({ text: String(reason), error: true });
    } finally {
      setBusy(false);
      reload();
    }
  };

  const remove = (id: string) => {
    setMessage(null);
    removeKnownHost(id)
      .catch((reason) => setMessage({ text: String(reason), error: true }))
      .finally(reload);
  };

  const shown = entries?.slice(0, VISIBLE) ?? [];

  return (
    <>
      <div className="flex min-h-11 items-center px-3 py-1.5">
        <Button variant="secondary" size="sm" disabled={busy} onClick={() => void doImport()}>
          {busy ? <LoaderCircle className="animate-spin motion-reduce:animate-none" /> : <Download />}
          {t.knownHostsImport}
        </Button>
      </div>
      {message && (
        <p
          className={
            message.error
              ? "flex items-start gap-1.5 bg-danger-soft px-3 py-2 text-callout text-danger"
              : "px-3 py-2 text-callout text-label-2"
          }
        >
          {message.error && <CircleAlert className="mt-0.5 size-3.5 shrink-0" strokeWidth={1.5} />}
          <span className="min-w-0 break-words">{message.text}</span>
        </p>
      )}
      {entries && entries.length === 0 ? (
        <p className="px-3 py-3 text-callout text-label-3">{t.knownHostsEmpty}</p>
      ) : (
        <ul className="max-h-72 divide-y divide-separator overflow-y-auto">
          {shown.map((entry) => (
            <li key={entry.id} className="flex items-center gap-2 px-3 py-1.5">
              <div className="min-w-0 flex-1">
                <p className="selectable truncate text-callout text-label-1" title={entry.hashed ? undefined : entry.host}>
                  {entry.hashed ? t.knownHostsHashed : entry.host}
                </p>
                <p className="selectable truncate font-mono text-callout text-label-3" title={entry.fingerprint}>
                  {entry.keyType} {entry.fingerprint}
                </p>
              </div>
              <Tooltip label={t.knownHostsRemove}>
                <Button
                  variant="quiet"
                  size="icon-sm"
                  aria-label={`${t.knownHostsRemove}: ${entry.hashed ? t.knownHostsHashed : entry.host}`}
                  onClick={() => remove(entry.id)}
                >
                  <Trash2 />
                </Button>
              </Tooltip>
            </li>
          ))}
        </ul>
      )}
      {entries && entries.length > VISIBLE && (
        <p className="px-3 py-2 text-callout text-label-3">
          {t.knownHostsMore} {entries.length - VISIBLE}
        </p>
      )}
      <p className="px-3 py-2 text-callout text-label-3">{t.knownHostsHint}</p>
    </>
  );
}
