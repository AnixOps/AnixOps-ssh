import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ChevronDown, ChevronUp, CircleAlert, Download, Eye, ScrollText, ShieldCheck, Trash2, X } from "lucide-react";
import { AnimatePresence } from "motion/react";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Overlay } from "@/components/ui/overlay";
import { Switch } from "@/components/ui/switch";
import { Tooltip } from "@/components/ui/tooltip";
import { logDelete, logExport, logList, logOpen, logSettingsGet, logSettingsSave } from "@/lib/aiBackend";
import { isVaultLocked } from "@/lib/backend";
import { t } from "@/lib/i18n";
import {
  formatDuration,
  formatSize,
  retentionOptions,
  type LogSettings,
  type OpenedLog,
  type SessionLogEntry,
} from "@/lib/sessionLogs";
import { settings as appSettings } from "@/lib/settings";
import { terminalFont, terminalTheme } from "@/lib/terminalTheme";
import { requestVaultUnlock, vaultErrorText } from "@/lib/vaultUi";

const units = () => ({ sec: t.logsSec, min: t.logsMin, hour: t.logsHour });

/** Settings → Session logs: the default, the retention, and the list of
 *  recorded sessions with a read-only viewer. The per-host choice is in the
 *  host editor. */
export function SessionLogs() {
  const [settings, setSettings] = useState<LogSettings | null>(null);
  const [entries, setEntries] = useState<SessionLogEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [viewing, setViewing] = useState<SessionLogEntry | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  const load = useCallback(() => {
    logSettingsGet().then(setSettings).catch((reason) => setError(String(reason)));
    logList().then(setEntries).catch((reason) => setError(String(reason)));
  }, []);
  useEffect(load, [load]);

  const save = (recordByDefault: boolean, retentionDays: number) => {
    setError(null);
    logSettingsSave(recordByDefault, retentionDays).then(setSettings).catch((reason) => setError(String(reason)));
  };

  const remove = (id: string) => {
    setConfirmDelete(null);
    logDelete(id)
      .then(() => setEntries((current) => current.filter((entry) => entry.id !== id)))
      .catch((reason) => setError(String(reason)));
  };

  if (!settings) {
    return <p className="px-3 py-2 text-caption font-normal text-label-3">{error ?? "…"}</p>;
  }

  return (
    <>
      <div className="flex items-start gap-2 px-3 py-2.5">
        <ShieldCheck className="mt-0.5 size-4 shrink-0 text-(--accent)" strokeWidth={1.5} />
        <p className="text-caption font-normal text-label-2">{t.logsIntro}</p>
      </div>

      <Row label={t.logsDefault} hint={t.logsDefaultHint}>
        <Switch
          on={settings.recordByDefault}
          label={t.logsDefault}
          onClick={() => save(!settings.recordByDefault, settings.retentionDays)}
        />
      </Row>
      <Row label={t.logsRetention}>
        <select
          aria-label={t.logsRetention}
          value={settings.retentionDays}
          onChange={(event) => save(settings.recordByDefault, Number(event.target.value))}
          className="h-7 rounded-xs bg-fill-1 px-2 text-callout text-label-1 outline-none focus-visible:ring-2 focus-visible:ring-(--accent)"
        >
          {retentionOptions(settings.retentionDays).map((days) => (
            <option key={days} value={days}>
              {days === 0 ? t.logsForever : `${days} ${t.logsDays}`}
            </option>
          ))}
        </select>
      </Row>

      {error && <p role="alert" className="selectable px-3 py-2 text-caption font-normal text-danger">{error}</p>}

      {entries.length === 0 ? (
        <p className="px-3 py-2.5 text-caption font-normal text-label-3">{t.logsEmpty}</p>
      ) : (
        <ul aria-label={t.logsTitle} className="divide-y divide-separator">
          {entries.map((entry) =>
            confirmDelete === entry.id ? (
              <li key={entry.id} className="space-y-2 px-3 py-2">
                <p className="text-caption font-normal text-label-1">{t.logsDeleteConfirm}</p>
                <div className="flex gap-2">
                  <Button variant="destructive" size="sm" onClick={() => remove(entry.id)}>
                    <Trash2 />
                    {t.logsDelete}
                  </Button>
                  <Button variant="secondary" size="sm" onClick={() => setConfirmDelete(null)}>
                    {t.cancel}
                  </Button>
                </div>
              </li>
            ) : (
              <li key={entry.id} className="flex items-center gap-2 px-3 py-2">
                <ScrollText className="size-4 shrink-0 text-label-3" strokeWidth={1.5} />
                <div className="min-w-0 flex-1">
                  <p className="truncate text-callout text-label-1">{entry.host}</p>
                  <p className="truncate text-caption font-normal text-label-2">
                    {new Date(entry.startedMs).toLocaleString()} · {formatDuration(entry.durationMs, units())} ·{" "}
                    {formatSize(entry.size)}
                  </p>
                </div>
                <Tooltip label={t.logsOpen}>
                  <Button variant="ghost" size="icon-sm" aria-label={t.logsOpen} onClick={() => setViewing(entry)}>
                    <Eye />
                  </Button>
                </Tooltip>
                <Tooltip label={t.logsDelete}>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t.logsDelete}
                    onClick={() => setConfirmDelete(entry.id)}
                  >
                    <Trash2 />
                  </Button>
                </Tooltip>
              </li>
            ),
          )}
        </ul>
      )}

      <AnimatePresence>
        {viewing && <LogViewer key={viewing.id} entry={viewing} onClose={() => setViewing(null)} />}
      </AnimatePresence>
    </>
  );
}

/** A decrypted log in a read-only terminal, with search and export. */
function LogViewer({ entry, onClose }: { entry: SessionLogEntry; onClose: () => void }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<SearchAddon | null>(null);
  const findRef = useRef<HTMLInputElement>(null);
  const [log, setLog] = useState<OpenedLog | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [confirmExport, setConfirmExport] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    logOpen(entry.id)
      .then((opened) => current && setLog(opened))
      .catch((reason) => {
        if (isVaultLocked(reason)) requestVaultUnlock();
        if (current) setError(vaultErrorText(reason));
      });
    return () => {
      current = false;
    };
  }, [entry.id]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || !log) return;
    const terminal = new Terminal({
      disableStdin: true,
      cursorBlink: false,
      cursorInactiveStyle: "none",
      fontFamily: terminalFont(),
      fontSize: appSettings().terminalFontSize,
      lineHeight: 1.2,
      scrollback: 100_000,
      theme: terminalTheme(),
    });
    const fit = new FitAddon();
    const search = new SearchAddon();
    terminal.loadAddon(fit);
    terminal.loadAddon(search);
    terminal.open(container);
    fit.fit();
    // The text is only ever written to the screen: nothing in it is run.
    terminal.write(`${log.text}\x1b[?25l`);
    searchRef.current = search;
    const observer = new ResizeObserver(() => {
      if (container.clientWidth > 0 && container.clientHeight > 0) fit.fit();
    });
    observer.observe(container);
    return () => {
      observer.disconnect();
      searchRef.current = null;
      terminal.dispose();
    };
  }, [log]);

  const find = (forward: boolean, incremental = false) => {
    if (!query) return;
    if (forward) searchRef.current?.findNext(query, { incremental });
    else searchRef.current?.findPrevious(query);
  };

  const exportText = async () => {
    setConfirmExport(false);
    try {
      if (await logExport(entry)) setNotice(t.logsExportDone);
    } catch (reason) {
      if (isVaultLocked(reason)) requestVaultUnlock();
      setError(vaultErrorText(reason));
    }
  };

  return (
    <Overlay label={`${t.logsTitle}: ${entry.host}`} onDismiss={onClose} className="flex h-[80vh] max-w-5xl flex-col overflow-hidden">
      <header className="flex shrink-0 flex-wrap items-center gap-2 px-4 py-3">
        <ScrollText className="size-4 text-label-2" strokeWidth={1.5} />
        <div className="min-w-0 flex-1">
          <h2 className="truncate text-callout font-medium">{entry.host}</h2>
          <p className="truncate text-caption font-normal text-label-2">
            {new Date(entry.startedMs).toLocaleString()} · {formatDuration(entry.durationMs, units())} · {t.logsReadOnly}
          </p>
        </div>
        <Input
          ref={findRef}
          value={query}
          placeholder={t.find}
          aria-label={t.find}
          className="h-7 w-48"
          onChange={(event) => {
            setQuery(event.target.value);
            if (event.target.value) searchRef.current?.findNext(event.target.value, { incremental: true });
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") find(!event.shiftKey);
          }}
        />
        <Tooltip label={t.findPrevious}>
          <Button variant="ghost" size="icon-sm" aria-label={t.findPrevious} onClick={() => find(false)}>
            <ChevronUp />
          </Button>
        </Tooltip>
        <Tooltip label={t.findNext}>
          <Button variant="ghost" size="icon-sm" aria-label={t.findNext} onClick={() => find(true)}>
            <ChevronDown />
          </Button>
        </Tooltip>
        <Button variant="secondary" size="sm" disabled={!log} onClick={() => setConfirmExport(true)}>
          <Download />
          {t.logsExport}
        </Button>
        <Tooltip label={t.logsClose}>
          <Button variant="ghost" size="icon-sm" aria-label={t.logsClose} onClick={onClose}>
            <X />
          </Button>
        </Tooltip>
      </header>

      {log && !log.complete && (
        <p className="mx-4 mb-2 flex items-start gap-2 rounded-sm bg-warning-soft px-3 py-2 text-caption font-normal text-warning">
          <CircleAlert className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} />
          <span>
            <span className="font-medium">{t.logsIncomplete}. </span>
            {t.logsIncompleteHint}
          </span>
        </p>
      )}
      {confirmExport && (
        <div role="alertdialog" aria-label={t.logsExportTitle} className="mx-4 mb-2 space-y-2 rounded-sm bg-fill-1 px-3 py-2">
          <p className="text-caption font-medium text-label-1">{t.logsExportTitle}</p>
          <p className="text-caption font-normal text-label-2">{t.logsExportConfirm}</p>
          <div className="flex gap-2">
            <Button variant="primary" size="sm" onClick={() => void exportText()}>
              <Download />
              {t.logsExport}
            </Button>
            <Button variant="secondary" size="sm" onClick={() => setConfirmExport(false)}>
              {t.cancel}
            </Button>
          </div>
        </div>
      )}
      {notice && <p role="status" className="mx-4 mb-2 text-caption font-normal text-label-2">{notice}</p>}
      {error && <p role="alert" className="selectable mx-4 mb-2 rounded-sm bg-danger-soft px-3 py-2 text-caption font-normal text-danger">{error}</p>}
      {!log && !error && <p className="mx-4 text-caption font-normal text-label-3">{t.logsLoading}…</p>}

      <div className="min-h-0 flex-1 bg-bg px-3 pt-2 pb-3">
        <div ref={containerRef} className="h-full w-full" />
      </div>
    </Overlay>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="flex min-h-11 items-center gap-3 px-3 py-1.5">
      <div className="min-w-0 flex-1">
        <p className="text-callout text-label-1">{label}</p>
        {hint && <p className="text-caption font-normal text-label-2">{hint}</p>}
      </div>
      {children}
    </div>
  );
}
