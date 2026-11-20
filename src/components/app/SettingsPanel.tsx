import { getVersion } from "@tauri-apps/api/app";
import { AiSettings } from "@/components/app/AiSettings";
import { SessionLogs } from "@/components/app/SessionLogs";
import { BackupSync } from "@/components/app/BackupSync";
import { KnownHosts } from "@/components/app/KnownHosts";
import { SyncSettings } from "@/components/app/SyncSettings";
import { VaultSettings } from "@/components/app/VaultSettings";
import { CircleAlert, CircleCheck, Eye, EyeOff, Minus, Plus, RotateCw, Trash2 } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import { SettingsRow, SettingsGroup } from "@/components/ui/settings";
import { Input } from "@/components/ui/input";
import { Segmented } from "@/components/ui/segmented";
import { Switch } from "@/components/ui/switch";
import { Tooltip } from "@/components/ui/tooltip";
import {
  isTauri,
  telemetryDeletePending,
  telemetryPending,
  telemetryStatus,
  type SshStatus,
} from "@/lib/backend";
import { completionPrefs, onCompletionPrefs, updateCompletionPrefs } from "@/lib/completion/prefs";
import { t } from "@/lib/i18n";
import type { LocaleSetting, ThemeSetting } from "@/lib/prefsMirror";
import { onSettings, settings, updateSettings, type Settings } from "@/lib/settings";
import { isMac } from "@/lib/shortcuts";
import {
  clampFont,
  LETTER_SPACING_MAX,
  LETTER_SPACING_MIN,
  LINE_HEIGHT_MAX,
  LINE_HEIGHT_MIN,
  SUGGESTED_FONTS,
  type TerminalFontSettings,
} from "@/lib/terminalFont";
import { pendingText, privacyView, type TelemetryStatus } from "@/lib/telemetry";
import { cn } from "@/lib/utils";

const SCROLLBACK = [1000, 5000, 10000, 50000, 100000];

export function SettingsPanel({
  status,
  onRecheck,
  onDataChanged,
}: {
  status: SshStatus | null;
  onRecheck: () => void;
  onDataChanged: () => void;
}) {
  const [current, setCurrent] = useState<Settings>(settings());
  const [prefs, setPrefs] = useState(completionPrefs());
  const [error, setError] = useState<string | null>(null);
  const [version, setVersion] = useState("");

  useEffect(() => onSettings(setCurrent), []);
  useEffect(() => onCompletionPrefs(setPrefs), []);
  useEffect(() => {
    if (isTauri) getVersion().then(setVersion).catch(console.error);
    else setVersion("preview");
  }, []);

  const update = (patch: Partial<Settings>) => {
    setError(null);
    updateSettings(patch).catch((reason) => setError(String(reason)));
  };

  const updateFont = (patch: Partial<TerminalFontSettings>) =>
    update({ terminalFont: clampFont({ ...current.terminalFont, ...patch }) });

  const themes: { value: ThemeSetting; label: string }[] = [
    { value: "system", label: t.themeSystem },
    { value: "light", label: t.themeLight },
    { value: "dark", label: t.themeDark },
  ];

  return (
    <aside className="material-sidebar flex w-72 shrink-0 flex-col border-r">
      <h2 className="px-4 pt-4 pb-2 text-title-3">{t.navSettings}</h2>
      <div className="min-h-0 flex-1 space-y-5 overflow-y-auto px-3 pb-4">
        {error && <p className="rounded-sm bg-danger-soft px-3 py-2 text-callout text-danger">{error}</p>}

        <SettingsGroup title={t.appearance}>
          <SettingsRow label={t.theme}>
            <Segmented
              size="sm"
              value={current.theme}
              onChange={(theme) => update({ theme })}
              label={t.theme}
              options={themes}
            />
          </SettingsRow>
          <SettingsRow label={t.language}>
            <Select
              aria-label={t.language}
              value={current.locale}
              onChange={(event) => update({ locale: event.target.value as LocaleSetting })}
              className="h-7 w-auto"
            >
              <option value="system">{t.languageSystem}</option>
              <option value="zh_cn">简体中文</option>
              <option value="en">English</option>
            </Select>
          </SettingsRow>
        </SettingsGroup>

        <SettingsGroup title={t.terminal}>
          <SettingsRow label={t.fontSize}>
            <div className="flex items-center gap-1">
              <Stepper
                label={`${t.fontSize} −`}
                disabled={current.terminalFontSize <= 9}
                onClick={() => update({ terminalFontSize: current.terminalFontSize - 1 })}
              >
                <Minus />
              </Stepper>
              <span className="w-7 text-center font-mono text-callout tabular-nums">{current.terminalFontSize}</span>
              <Stepper
                label={`${t.fontSize} +`}
                disabled={current.terminalFontSize >= 32}
                onClick={() => update({ terminalFontSize: current.terminalFontSize + 1 })}
              >
                <Plus />
              </Stepper>
            </div>
          </SettingsRow>
          <SettingsRow label={t.fontFamily} hint={t.fontFamilyHint}>
            <FontFamilyField
              value={current.terminalFont.family}
              onCommit={(family) => updateFont({ family })}
            />
          </SettingsRow>
          <SettingsRow label={t.lineHeight}>
            <div className="flex items-center gap-1">
              <Stepper
                label={`${t.lineHeight} −`}
                disabled={current.terminalFont.lineHeight <= LINE_HEIGHT_MIN}
                onClick={() => updateFont({ lineHeight: current.terminalFont.lineHeight - 0.1 })}
              >
                <Minus />
              </Stepper>
              <span className="w-9 text-center font-mono text-callout tabular-nums">
                {current.terminalFont.lineHeight.toFixed(1)}
              </span>
              <Stepper
                label={`${t.lineHeight} +`}
                disabled={current.terminalFont.lineHeight >= LINE_HEIGHT_MAX}
                onClick={() => updateFont({ lineHeight: current.terminalFont.lineHeight + 0.1 })}
              >
                <Plus />
              </Stepper>
            </div>
          </SettingsRow>
          <SettingsRow label={t.letterSpacing}>
            <div className="flex items-center gap-1">
              <Stepper
                label={`${t.letterSpacing} −`}
                disabled={current.terminalFont.letterSpacing <= LETTER_SPACING_MIN}
                onClick={() => updateFont({ letterSpacing: current.terminalFont.letterSpacing - 1 })}
              >
                <Minus />
              </Stepper>
              <span className="w-9 text-center font-mono text-callout tabular-nums">{current.terminalFont.letterSpacing}</span>
              <Stepper
                label={`${t.letterSpacing} +`}
                disabled={current.terminalFont.letterSpacing >= LETTER_SPACING_MAX}
                onClick={() => updateFont({ letterSpacing: current.terminalFont.letterSpacing + 1 })}
              >
                <Plus />
              </Stepper>
            </div>
          </SettingsRow>
          <SettingsRow label={t.scrollback}>
            <Select
              aria-label={t.scrollback}
              value={current.terminalScrollback}
              onChange={(event) => update({ terminalScrollback: Number(event.target.value) })}
              className="h-7 w-auto"
            >
              {SCROLLBACK.map((lines) => (
                <option key={lines} value={lines}>
                  {lines.toLocaleString()} {t.lines}
                </option>
              ))}
            </Select>
          </SettingsRow>
          <SettingsRow
            label={t.rightClick}
            hint={current.terminalRightClick === "paste" ? t.rightClickPasteHint : undefined}
          >
            <Segmented
              size="sm"
              value={current.terminalRightClick}
              onChange={(terminalRightClick) => update({ terminalRightClick })}
              label={t.rightClick}
              options={[
                { value: "menu", label: t.rightClickMenu },
                { value: "paste", label: t.rightClickPaste },
              ]}
            />
          </SettingsRow>
          <SettingsRow label={t.copyOnSelect}>
            <Switch
              on={current.terminalCopyOnSelect}
              label={t.copyOnSelect}
              onClick={() => update({ terminalCopyOnSelect: !current.terminalCopyOnSelect })}
            />
          </SettingsRow>
          <SettingsRow label={t.warnMultilinePaste} hint={t.warnMultilinePasteHint}>
            <Switch
              on={current.terminalWarnMultilinePaste}
              label={t.warnMultilinePaste}
              onClick={() => update({ terminalWarnMultilinePaste: !current.terminalWarnMultilinePaste })}
            />
          </SettingsRow>
          <SettingsRow label={t.osc52} hint={t.osc52Hint}>
            <Switch
              on={current.terminalOsc52}
              label={t.osc52}
              onClick={() => update({ terminalOsc52: !current.terminalOsc52 })}
            />
          </SettingsRow>
          {isMac && (
            <SettingsRow label={t.optionAsMeta} hint={t.optionAsMetaHint}>
              <Switch
                on={current.terminalOptionAsMeta}
                label={t.optionAsMeta}
                onClick={() => update({ terminalOptionAsMeta: !current.terminalOptionAsMeta })}
              />
            </SettingsRow>
          )}
        </SettingsGroup>

        <SettingsGroup title={t.completion}>
          <SettingsRow label={t.completionEnabled} hint={t.completionHint}>
            <Switch on={prefs.enabled} label={t.completionEnabled} onClick={() => updateCompletionPrefs({ enabled: !prefs.enabled })} />
          </SettingsRow>
          <SettingsRow label={t.rememberHistory} hint={t.rememberHistoryHint}>
            <Switch
              on={prefs.rememberHistory}
              label={t.rememberHistory}
              onClick={() => updateCompletionPrefs({ rememberHistory: !prefs.rememberHistory })}
            />
          </SettingsRow>
        </SettingsGroup>

        <SettingsGroup
          title={t.sshEnvironment}
          action={
            <Tooltip label={t.recheck}>
              <Button variant="quiet" size="icon-sm" aria-label={t.recheck} onClick={onRecheck}>
                <RotateCw />
              </Button>
            </Tooltip>
          }
        >
          <StatusRow
            label={t.openssh}
            ok={!status || Boolean(status.sshVersion)}
            value={status ? (status.sshVersion?.split(",")[0] ?? t.notFound) : "…"}
          />
          <StatusRow label={t.sftpLabel} ok={!status || status.sftpFound} value={status ? (status.sftpFound ? t.available : t.notFound) : "…"} />
          <StatusRow
            label={t.agentLabel}
            ok={!status || status.agent === "ready"}
            warn={status?.agent === "empty"}
            value={
              !status
                ? "…"
                : status.agent === "ready"
                  ? `${status.agentKeys.length} ${t.agentReady}`
                  : status.agent === "empty"
                    ? t.agentEmpty
                    : t.agentUnavailable
            }
          />
          <StatusRow label={t.sshConfigLabel} ok value={status ? `${status.sshConfigHosts} ${t.hostsCount}` : "…"} />
          {status && status.agentKeys.length > 0 && (
            <div className="space-y-1 px-3 py-2">
              {status.agentKeys.map((key) => (
                <p key={key} className="selectable truncate font-mono text-callout text-label-3" title={key}>
                  {key}
                </p>
              ))}
            </div>
          )}
        </SettingsGroup>

        <SettingsGroup title={t.security}>
          <VaultSettings />
        </SettingsGroup>

        <SettingsGroup title={t.aiTitle}>
          <AiSettings />
        </SettingsGroup>

        <SettingsGroup title={t.logsTitle}>
          <SessionLogs />
        </SettingsGroup>

        <SettingsGroup title={t.privacy}>
          <PrivacySection current={current} update={update} />
        </SettingsGroup>

        <SettingsGroup title={t.syncTitle}>
          <SyncSettings />
        </SettingsGroup>

        <SettingsGroup title={t.backupSync}>
          <BackupSync onChanged={onDataChanged} />
        </SettingsGroup>

        <SettingsGroup title={t.knownHosts}>
          <KnownHosts />
        </SettingsGroup>

        <SettingsGroup title={t.advanced}>
          <SettingsRow label={t.sshEngineLabel} hint={current.sshEngine === "russh" ? t.sshEngineRusshHint : t.sshEngineOpensshHint}>
            <Segmented
              size="sm"
              value={current.sshEngine}
              onChange={(sshEngine) => update({ sshEngine })}
              label={t.sshEngineLabel}
              options={[
                { value: "russh", label: t.engineBuiltin },
                { value: "openssh", label: t.engineOpenssh },
              ]}
            />
          </SettingsRow>
        </SettingsGroup>

        <SettingsGroup title={t.about}>
          <SettingsRow label="AnixOps SSH">
            <span className="text-callout text-label-2">
              {t.version} {version}
            </span>
          </SettingsRow>
        </SettingsGroup>
      </div>
    </aside>
  );
}

function StatusRow({ label, value, ok, warn = false }: { label: string; value: string; ok: boolean; warn?: boolean }) {
  return (
    <SettingsRow label={label}>
      <span
        className={cn(
          "flex min-w-0 items-center gap-1.5 text-callout",
          ok ? "text-label-2" : warn ? "text-warning" : "text-danger",
        )}
      >
        {ok ? (
          <CircleCheck className="size-3.5 shrink-0 text-success" strokeWidth={1.5} />
        ) : (
          <CircleAlert className="size-3.5 shrink-0" strokeWidth={1.5} />
        )}
        <span className="truncate">{value}</span>
      </span>
    </SettingsRow>
  );
}

function Stepper({
  label,
  disabled,
  onClick,
  children,
}: {
  label: string;
  disabled: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <Button variant="secondary" size="icon-sm" aria-label={label} disabled={disabled} onClick={onClick}>
      {children}
    </Button>
  );
}

function PrivacySection({ current, update }: { current: Settings; update: (patch: Partial<Settings>) => void }) {
  const [status, setStatus] = useState<TelemetryStatus | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = () => telemetryStatus().then(setStatus).catch(console.error);
  useEffect(() => {
    refresh();
  }, [current.crashReports]);

  const view = privacyView(current, status);

  const toggleView = () => {
    if (pending !== null) {
      setPending(null);
      return;
    }
    setNotice(null);
    telemetryPending()
      .then((found) => setPending(pendingText(found) || t.pendingNone))
      .catch(console.error);
  };

  const remove = () => {
    telemetryDeletePending()
      .then(() => {
        setPending(null);
        setNotice(t.pendingDeleted);
        return refresh();
      })
      .catch(console.error);
  };

  if (view.nothingCollected && status) {
    return <p className="px-3 py-2 text-callout text-label-2">{t.telemetryUnavailable}</p>;
  }

  return (
    <>
      <SettingsRow label={t.crashReports} hint={t.crashReportsHint}>
        <Switch
          on={view.crashOn}
          label={t.crashReports}
          onClick={() => update({ crashReports: !view.crashOn })}
        />
      </SettingsRow>
      <SettingsRow label={t.usageStats} hint={view.usageAvailable ? t.usageStatsHint : t.usageUnavailable}>
        <Switch
          on={view.usageOn}
          label={t.usageStats}
          onClick={() => view.usageAvailable && update({ usageStats: !view.usageOn })}
        />
      </SettingsRow>
      <div className="space-y-2 px-3 py-2">
        <div className="flex flex-wrap gap-2">
          <Button variant="secondary" size="sm" onClick={toggleView}>
            {pending === null ? <Eye /> : <EyeOff />}
            {pending === null ? t.viewPending : t.hidePending}
          </Button>
          <Button variant="secondary" size="sm" onClick={remove} disabled={!status || status.pending === 0}>
            <Trash2 />
            {t.deletePending}
          </Button>
        </div>
        {status && status.pending > 0 && (
          <p className="text-callout text-label-2">
            {status.pending} {t.pendingCount}
          </p>
        )}
        {notice && <p className="text-callout text-label-2">{notice}</p>}
        {pending !== null && (
          <pre
            aria-label={t.viewPending}
            className="selectable max-h-48 overflow-auto rounded-xs bg-fill-1 p-2 font-mono text-callout whitespace-pre-wrap break-all text-label-2"
          >
            {pending}
          </pre>
        )}
      </div>
    </>
  );
}

/** The font name is saved when the field loses focus or Enter is pressed, not on every key. */
function FontFamilyField({ value, onCommit }: { value: string; onCommit: (family: string) => void }) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  const commit = () => text !== value && onCommit(text);
  return (
    <>
      <Input
        list="terminal-font-suggestions"
        aria-label={t.fontFamily}
        value={text}
        placeholder={t.fontFamilyDefault}
        onChange={(event) => setText(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => event.key === "Enter" && commit()}
        className="h-7 w-48"
      />
      <datalist id="terminal-font-suggestions">
        {SUGGESTED_FONTS.map((name) => (
          <option key={name} value={name} />
        ))}
      </datalist>
    </>
  );
}
