import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ChevronDown, ChevronUp, Download, Eye, ScrollText, ShieldCheck, Trash2, X } from "lucide-react";
import { AnimatePresence } from "motion/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { Menu } from "@/components/ui/menu";
import { Select } from "@/components/ui/select";
import { SettingsRow } from "@/components/ui/settings";
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
import { disposeTerminal } from "@/lib/terminalOptions";
import { terminalFont, terminalTheme, useTerminalScheme } from "@/lib/terminalTheme";
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
  const confirm = useConfirm();

  const load = useCallback(() => {
    logSettingsGet().then(setSettings).catch((reason) => setError(String(reason)));
    logList().then(setEntries).catch((reason) => setError(String(reason)));
  }, []);
  useEffect(load, [load]);

  const save = (recordByDefault: boolean, retentionDays: number) => {
    setError(null);
    logSettingsSave(recordByDefault, retentionDays).then(setSettings).catch((reason) => setError(String(reason)));
  };

  const remove = async (entry: SessionLogEntry) => {
    const title = t.logDeleteTitle.replace("{host}", entry.host);
    if (!(await confirm({ title, body: t.logsDeleteConfirm, verb: t.logsDelete }))) return;
    logDelete(entry.id)
      .then(() => setEntries((current) => current.filter((item) => item.id !== entry.id)))
      .catch((reason) => setError(String(reason)));
  };

  if (!settings) {
    return <p className="px-3 py-2 text-callout text-label-3">{error ?? "…"}</p>;
  }

  return (
    <>
      <div className="flex items-start gap-2 px-3 py-2.5">
        <ShieldCheck className="mt-0.5 size-4 shrink-0 text-(--accent)" strokeWidth={1.5} />
        <p className="text-callout text-label-2">{t.logsIntro}</p>
      </div>

      <SettingsRow label={t.logsDefault} hint={t.logsDefaultHint}>
        <Switch
          on={settings.recordByDefault}
          label={t.logsDefault}
          onClick={() => save(!settings.recordByDefault, settings.retentionDays)}
        />
      </SettingsRow>
      <SettingsRow label={t.logsRetention}>
        <Select
          aria-label={t.logsRetention}
          value={settings.retentionDays}
          onChange={(event) => save(settings.recordByDefault, Number(event.target.value))}
          className="h-7 w-auto"
        >
          {retentionOptions(settings.retentionDays).map((days) => (
            <option key={days} value={days}>
              {days === 0 ? t.logsForever : `${days} ${t.logsDays}`}
            </option>
          ))}
        </Select>
      </SettingsRow>

      {error && <Notice className="m-3">{error}</Notice>}

      {entries.length === 0 ? (
        <p className="px-3 py-2.5 text-callout text-label-3">{t.logsEmpty}</p>
      ) : (
        <ul aria-label={t.logsTitle} className="divide-y divide-separator">
          {entries.map((entry) => (
            <li key={entry.id} className="flex items-center gap-2 px-4 py-2">
              <ScrollText className="size-4 shrink-0 text-label-3" strokeWidth={1.5} />
              <div className="min-w-0 flex-1">
                <p className="truncate text-body text-label-1" title={entry.host}>
                  {entry.host}
                </p>
                <p className="truncate text-callout text-label-2 tabular-nums">
                  {new Date(entry.startedMs).toLocaleString()} · {formatDuration(entry.durationMs, units())} ·{" "}
                  {formatSize(entry.size)}
                </p>
              </div>
              <Tooltip label={t.logsOpen}>
                <Button variant="quiet" size="icon-sm" aria-label={t.logsOpen} onClick={() => setViewing(entry)}>
                  <Eye />
                </Button>
              </Tooltip>
              <Menu
                label={t.more}
                items={[{ key: "delete", label: t.logsDelete, icon: <Trash2 />, danger: true, onSelect: () => void remove(entry) }]}
              />
            </li>
          ))}
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
  const schemeRef = useRef<HTMLDivElement>(null);
  useTerminalScheme(schemeRef);
  const searchRef = useRef<SearchAddon | null>(null);
  const findRef = useRef<HTMLInputElement>(null);
  const [log, setLog] = useState<OpenedLog | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const confirm = useConfirm();
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
      disposeTerminal(terminal);
    };
  }, [log]);

  const find = (forward: boolean, incremental = false) => {
    if (!query) return;
    if (forward) searchRef.current?.findNext(query, { incremental });
    else searchRef.current?.findPrevious(query);
  };

  const exportText = async () => {
    if (!(await confirm({ title: t.logsExportTitle, body: t.logsExportConfirm, verb: t.logsExport, destructive: false }))) return;
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
          <h2 className="truncate text-title-3">{entry.host}</h2>
          <p className="truncate text-callout text-label-2">
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
          <Button variant="quiet" size="icon-sm" aria-label={t.findPrevious} onClick={() => find(false)}>
            <ChevronUp />
          </Button>
        </Tooltip>
        <Tooltip label={t.findNext}>
          <Button variant="quiet" size="icon-sm" aria-label={t.findNext} onClick={() => find(true)}>
            <ChevronDown />
          </Button>
        </Tooltip>
        <Button variant="secondary" size="sm" disabled={!log} onClick={() => void exportText()}>
          <Download />
          {t.logsExport}
        </Button>
        <Tooltip label={t.logsClose}>
          <Button variant="quiet" size="icon-sm" aria-label={t.logsClose} onClick={onClose}>
            <X />
          </Button>
        </Tooltip>
      </header>

      {log && !log.complete && (
        <Notice tone="warning" className="mx-4 mb-2">
          {t.logsIncomplete}. {t.logsIncompleteHint}
        </Notice>
      )}
      {notice && <p role="status" className="mx-4 mb-2 text-callout text-label-2">{notice}</p>}
      {error && <Notice className="mx-4 mb-2">{error}</Notice>}
      {!log && !error && <p className="mx-4 text-callout text-label-3">{t.logsLoading}…</p>}

      <div ref={schemeRef} className="min-h-0 flex-1 bg-bg px-3 pt-2 pb-3">
        <div ref={containerRef} className="h-full w-full" />
      </div>
    </Overlay>
  );
}
