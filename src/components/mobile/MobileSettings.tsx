import { ChevronRight, Keyboard, Minus, Plus } from "lucide-react";
import { useEffect, useId, useState, type ReactNode } from "react";
import { ShortcutList } from "@/components/app/ShortcutList";
import { MobileBarEditor } from "@/components/mobile/MobileBarEditor";
import { MobileSwitch } from "@/components/mobile/MobileSwitch";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import { SettingsGroup, SettingsRow } from "@/components/ui/settings";
import { hasSessionNotification } from "@/lib/androidBridge";
import { GRACE_CHOICES, KEEPALIVE_CHOICES } from "@/lib/backgroundPolicy";
import { t } from "@/lib/i18n";
import type { ImeMode } from "@/lib/imeMachine";
import { setHostAttach, updateMobilePrefs, useMobilePrefs, type Attach } from "@/lib/mobilePrefs";
import { MAX_FONT, MIN_FONT } from "@/lib/touchGestures";

const GRACE_LABELS: Record<number, string> = {
  0: t.mobileGraceNow,
  30: t.mobileGrace30,
  60: t.mobileGrace60,
  300: t.mobileGrace300,
  900: t.mobileGrace900,
  [-1]: t.mobileGraceLong,
};

const KEEPALIVE_LABELS: Record<number, string> = {
  0: t.mobileKeepaliveOff,
  15: t.mobileKeepalive15,
  30: t.mobileKeepalive30,
  60: t.mobileKeepalive60,
};

/** A row with a label, a hint and a native select under them (phone pattern:
 *  the choice gets the full width, the platform draws the list). */
function SelectRow<T extends string | number>({
  label,
  hint,
  value,
  options,
  onChange,
}: {
  label: string;
  hint?: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (value: T) => void;
}) {
  const id = useId();
  return (
    <div className="flex flex-col gap-2 px-4 py-3">
      <label htmlFor={id} className="text-body text-label-1">
        {label}
      </label>
      <Select
        id={id}
        className="h-11"
        value={String(value)}
        onChange={(event) => {
          const chosen = options.find((option) => String(option.value) === event.target.value);
          if (chosen) onChange(chosen.value);
        }}
      >
        {options.map((option) => (
          <option key={String(option.value)} value={String(option.value)}>
            {option.label}
          </option>
        ))}
      </Select>
      {hint && <p className="text-callout text-label-2">{hint}</p>}
    </div>
  );
}

function SwitchRow({ label, hint, on, onToggle }: { label: string; hint?: string; on: boolean; onToggle: () => void }) {
  return (
    <SettingsRow label={label} hint={hint}>
      <MobileSwitch on={on} label={label} onToggle={onToggle} />
    </SettingsRow>
  );
}

function NavRow({ label, hint, icon, onClick }: { label: string; hint?: string; icon?: ReactNode; onClick: () => void }) {
  return (
    <button type="button" onClick={onClick} className="flex min-h-11 w-full items-center gap-3 px-4 py-2 text-left active:bg-fill-1">
      {icon}
      <span className="min-w-0 flex-1">
        <span className="block text-body text-label-1">{label}</span>
        {hint && <span className="block text-callout text-label-2">{hint}</span>}
      </span>
      <ChevronRight className="size-5 shrink-0 text-label-3" strokeWidth={1.5} />
    </button>
  );
}

/** The phone's settings: a full screen with grouped rows (density.md N23) that
 *  slides over the terminal. `host` adds the per-host choices. */
export function MobileSettings({
  host,
  onClose,
}: {
  host: { id: string; name: string } | null;
  onClose: () => void;
}) {
  const prefs = useMobilePrefs();
  const [screen, setScreen] = useState<"main" | "bar" | "shortcuts">("main");

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && screen === "main") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, screen]);

  if (screen === "bar") return <MobileBarEditor onBack={() => setScreen("main")} />;

  const attach: Attach = host ? (prefs.attach[host.id] ?? "off") : "off";
  const setFont = (delta: number) => updateMobilePrefs({ fontSize: Math.min(MAX_FONT, Math.max(MIN_FONT, prefs.fontSize + delta)) });

  return (
    <div role="dialog" aria-modal="true" aria-labelledby="mobile-settings-title" className="fixed inset-0 z-drawer flex flex-col bg-bg-grouped">
      <header className="material-bar pt-safe shrink-0 border-b">
        <div className="px-safe flex h-14 items-center justify-between gap-2">
          <h2 id="mobile-settings-title" className="truncate text-title-3">
            {t.mobileSettingsTitle}
          </h2>
          <Button variant="quiet" size="lg" onClick={onClose}>
            {t.close}
          </Button>
        </div>
      </header>
      <div className="px-safe flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto py-5 pb-safe">
        <SettingsGroup title={t.mobileSecTerminal}>
          <SettingsRow label={t.fontSize} hint={t.mobileFontSizeHint}>
            <div className="flex items-center gap-1">
              <Button variant="quiet" size="icon-lg" aria-label={t.mobileFontSmaller} disabled={prefs.fontSize <= MIN_FONT} onClick={() => setFont(-1)}>
                <Minus />
              </Button>
              <span aria-live="polite" className="w-8 text-center text-body tabular-nums">
                {prefs.fontSize}
              </span>
              <Button variant="quiet" size="icon-lg" aria-label={t.mobileFontLarger} disabled={prefs.fontSize >= MAX_FONT} onClick={() => setFont(1)}>
                <Plus />
              </Button>
            </div>
          </SettingsRow>
          <SelectRow<ImeMode>
            label={t.mobileIme}
            hint={t.mobileImeHint}
            value={prefs.imeMode}
            options={[
              { value: "auto", label: t.mobileImeAuto },
              { value: "live", label: t.mobileImeLive },
              { value: "hold", label: t.mobileImeHold },
            ]}
            onChange={(imeMode) => updateMobilePrefs({ imeMode })}
          />
          <SwitchRow label={t.mobileHaptics} hint={t.mobileHapticsHint} on={prefs.haptics} onToggle={() => updateMobilePrefs({ haptics: !prefs.haptics })} />
        </SettingsGroup>

        <SettingsGroup title={t.mobileSecKeys}>
          <SwitchRow
            label={t.mobileShowExtraRow}
            hint={t.mobileShowExtraRowHint}
            on={prefs.extraRow}
            onToggle={() => updateMobilePrefs({ extraRow: !prefs.extraRow })}
          />
          <NavRow label={t.mobileCustomiseBar} hint={t.mobileCustomiseBarHint} icon={<Keyboard className="size-5 text-label-2" strokeWidth={1.5} />} onClick={() => setScreen("bar")} />
          <NavRow label={t.shortcuts} hint={t.mobileShortcutsHint} onClick={() => setScreen("shortcuts")} />
        </SettingsGroup>

        <SettingsGroup title={t.mobileSecConnection}>
          <p className="px-4 py-3 text-callout text-label-2">{t.mobileBackgroundExplain}</p>
          <SelectRow<number>
            label={t.mobileGrace}
            hint={t.mobileGraceHint}
            value={prefs.graceSeconds}
            options={GRACE_CHOICES.map((seconds) => ({ value: seconds, label: GRACE_LABELS[seconds] }))}
            onChange={(graceSeconds) => updateMobilePrefs({ graceSeconds })}
          />
          <SelectRow<number>
            label={t.mobileKeepalive}
            hint={t.mobileKeepaliveHint}
            value={prefs.keepaliveSeconds}
            options={KEEPALIVE_CHOICES.map((seconds) => ({ value: seconds, label: KEEPALIVE_LABELS[seconds] }))}
            onChange={(keepaliveSeconds) => updateMobilePrefs({ keepaliveSeconds })}
          />
          {hasSessionNotification() && (
            <SwitchRow
              label={t.mobileForeground}
              hint={t.mobileForegroundHint}
              on={prefs.foregroundService}
              onToggle={() => updateMobilePrefs({ foregroundService: !prefs.foregroundService })}
            />
          )}
        </SettingsGroup>

        {host && (
          <SettingsGroup title={`${t.mobileSecHost}: ${host.name}`}>
            <SelectRow<Attach>
              label={t.mobileAttach}
              hint={t.mobileAttachHint}
              value={attach}
              options={[
                { value: "off", label: t.mobileAttachOff },
                { value: "tmux", label: "tmux" },
                { value: "screen", label: "screen" },
              ]}
              onChange={(value) => setHostAttach(host.id, value)}
            />
          </SettingsGroup>
        )}

        <SettingsGroup title={t.mobileSecAccessibility}>
          <SwitchRow
            label={t.mobileScreenReader}
            hint={t.mobileScreenReaderHint}
            on={prefs.screenReader}
            onToggle={() => updateMobilePrefs({ screenReader: !prefs.screenReader })}
          />
        </SettingsGroup>
      </div>
      {screen === "shortcuts" && <ShortcutList onClose={() => setScreen("main")} only={MOBILE_SHORTCUTS} />}
    </div>
  );
}

/** The shortcuts that mean something in the touch app when a hardware keyboard is attached. */
export const MOBILE_SHORTCUTS = ["search-hosts", "shortcut-list", "settings", "go-hosts", "new-host", "close-tab", "copy", "paste"] as const;
