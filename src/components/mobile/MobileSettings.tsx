import {
  ArrowLeft,
  ChevronRight,
  Fingerprint,
  Keyboard,
  Lock,
  LockOpen,
  Minus,
  Plus,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import { useEffect, useId, useState, type FormEvent, type ReactNode } from "react";
import { ShortcutList } from "@/components/app/ShortcutList";
import { MobileBarEditor } from "@/components/mobile/MobileBarEditor";
import { MobileSwitch } from "@/components/mobile/MobileSwitch";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { SettingsGroup, SettingsRow } from "@/components/ui/settings";
import {
  vaultDisableUnlock,
  vaultEnableUnlock,
  vaultForgetAll,
  vaultLock,
  vaultRemovePassphrase,
  vaultSetBackgroundLock,
  vaultSetPassphrase,
  type VaultStatus,
} from "@/lib/backend";
import { hasSessionNotification } from "@/lib/androidBridge";
import { GRACE_CHOICES, KEEPALIVE_CHOICES } from "@/lib/backgroundPolicy";
import { t } from "@/lib/i18n";
import type { ImeMode } from "@/lib/imeMachine";
import { setHostAttach, updateMobilePrefs, useMobilePrefs, type Attach } from "@/lib/mobilePrefs";
import { MAX_FONT, MIN_FONT } from "@/lib/touchGestures";
import {
  BIOMETRIC,
  backgroundLockChoices,
  backgroundLockLabel,
  biometricMethod,
  passphraseProblem,
  passphraseReady,
} from "@/lib/mobileVault";
import { vaultErrorText } from "@/lib/vaultUi";

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
      <MobileSwitch on={on} label={label} onClick={onToggle} />
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

type Editing = "set" | "change" | "remove" | "biometric" | null;

/** The phone's settings: the terminal, keys and background policy, then the
 *  vault. A full screen with Back on the left. `host` adds the choices for the
 *  host whose terminal is open. Forms open under their row with the actions
 *  stacked at full width (actions.md O5). */
export function MobileSettings({
  status,
  onStatus,
  onBack,
  host = null,
}: {
  status: VaultStatus;
  onStatus: (status: VaultStatus) => void;
  onBack: () => void;
  host?: { id: string; name: string } | null;
}) {
  const prefs = useMobilePrefs();
  const [screen, setScreen] = useState<"main" | "bar" | "shortcuts">("main");
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && screen === "main") onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack, screen]);
  const attach: Attach = host ? (prefs.attach[host.id] ?? "off") : "off";
  const setFont = (delta: number) =>
    updateMobilePrefs({ fontSize: Math.min(MAX_FONT, Math.max(MIN_FONT, prefs.fontSize + delta)) });
  const [editing, setEditing] = useState<Editing>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const confirm = useConfirm();
  const biometrics = biometricMethod(status);
  const biometricsOn = biometrics?.enabled === true;
  const seconds = status.backgroundLockSeconds ?? 0;

  const run = async (action: () => Promise<VaultStatus>) => {
    setError(null);
    setNotice(null);
    try {
      onStatus(await action());
      return true;
    } catch (reason) {
      setError(vaultErrorText(reason));
      return false;
    }
  };

  const toggleBiometrics = () => {
    if (biometricsOn) void run(() => vaultDisableUnlock(BIOMETRIC));
    else setEditing(editing === "biometric" ? null : "biometric");
  };

  if (screen === "bar") return <MobileBarEditor onBack={() => setScreen("main")} />;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="mobile-settings-title"
      className="fixed inset-0 z-drawer flex flex-col bg-bg-grouped"
    >
      <header className="material-bar pt-safe shrink-0 border-b">
        <div className="px-safe flex h-14 items-center gap-2">
          <Button
            variant="quiet"
            size="icon-lg"
            aria-label={t.mobileSettingsBack}
            onClick={onBack}
          >
            <ArrowLeft />
          </Button>
          <h2 id="mobile-settings-title" className="truncate text-title-3">
            {t.mobileSettings}
          </h2>
        </div>
      </header>
      <div className="px-safe pb-safe flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto py-4">
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

        <div className="flex items-start gap-3">
          <ShieldCheck
            className="mt-0.5 size-5 shrink-0 text-(--accent)"
            strokeWidth={1.5}
            aria-hidden
          />
          <p className="text-callout text-label-2">
            {status.passphraseSet
              ? t.vaultIntroPassphrase
              : t.mvDeviceOnlyIntro}
          </p>
        </div>
        {!status.keychainAvailable && (
          <Notice tone="warning">
            {t.vaultKeychainMissing}
            {status.keychainError ? `: ${status.keychainError}` : ""}
          </Notice>
        )}

        <SettingsGroup title={t.mvSecurity}>
          {status.passphraseSet && (
            <SettingsRow
              label={t.vaultState}
              hint={status.unlocked ? t.vaultUnlocked : t.vaultLocked}
            >
              {status.unlocked ? (
                <Button
                  variant="secondary"
                  size="lg"
                  onClick={() => void run(vaultLock)}
                >
                  <Lock />
                  {t.vaultLockNow}
                </Button>
              ) : (
                <LockOpen
                  className="size-5 text-label-3"
                  strokeWidth={1.5}
                  aria-hidden
                />
              )}
            </SettingsRow>
          )}

          <SettingsRow
            label={t.vaultPassphrase}
            hint={
              status.passphraseSet
                ? t.vaultPassphraseSet
                : t.vaultPassphraseUnset
            }
          >
            <Button
              variant="secondary"
              size="lg"
              disabled={!status.keychainAvailable}
              onClick={() => {
                const target = status.passphraseSet ? "change" : "set";
                setEditing(editing === target ? null : target);
              }}
            >
              {status.passphraseSet
                ? t.vaultChangePassphrase
                : t.vaultSetPassphrase}
            </Button>
          </SettingsRow>
          {status.passphraseSet && (
            <div className="px-4 py-2">
              <Button
                variant="danger"
                size="lg"
                className="w-full"
                onClick={() =>
                  setEditing(editing === "remove" ? null : "remove")
                }
              >
                {t.vaultRemovePassphrase}
              </Button>
            </div>
          )}
          {(editing === "set" ||
            editing === "change" ||
            editing === "remove") && (
            <PassphraseForm
              mode={editing}
              onCancel={() => setEditing(null)}
              onSubmit={async (current, next) => {
                const done = await run(() =>
                  editing === "remove"
                    ? vaultRemovePassphrase(current)
                    : vaultSetPassphrase(
                        next,
                        editing === "change" ? current : null,
                      ),
                );
                if (done) setEditing(null);
              }}
            />
          )}

          <SettingsRow
            label={t.unlockBiometric}
            hint={
              !biometrics
                ? t.mvBiometricUnavailable
                : !status.passphraseSet
                  ? t.mvBiometricNeedsPassphrase
                  : t.mvBiometricHint
            }
          >
            <MobileSwitch
              on={biometricsOn}
              label={t.unlockBiometric}
              disabled={!biometrics || !status.passphraseSet}
              onClick={toggleBiometrics}
            />
          </SettingsRow>
          {editing === "biometric" && !biometricsOn && (
            <PassphraseConfirm
              onCancel={() => setEditing(null)}
              onSubmit={async (passphrase) => {
                if (await run(() => vaultEnableUnlock(BIOMETRIC, passphrase)))
                  setEditing(null);
              }}
            />
          )}

          <SettingsRow label={t.mvBackgroundLock} hint={t.mvBackgroundLockHint}>
            <Select
              aria-label={t.mvBackgroundLock}
              value={seconds}
              disabled={!status.passphraseSet}
              onChange={(event) =>
                void run(() =>
                  vaultSetBackgroundLock(Number(event.target.value)),
                )
              }
              className="h-11 w-auto max-w-40"
            >
              {backgroundLockChoices(seconds).map((choice) => (
                <option key={choice} value={choice}>
                  {backgroundLockLabel(choice)}
                </option>
              ))}
            </Select>
          </SettingsRow>

          <SettingsRow label={t.vaultForgetAll} hint={t.vaultForgetHint}>
            <Button
              variant="danger"
              size="lg"
              onClick={async () => {
                const ok = await confirm({
                  title: t.vaultForgetTitle,
                  body: t.vaultForgetBody,
                  verb: t.vaultForgetButton,
                  typed: t.vaultForgetWord,
                });
                if (!ok) return;
                setError(null);
                vaultForgetAll()
                  .then((hosts) => setNotice(`${t.vaultForgotten}: ${hosts}`))
                  .catch((reason) => setError(vaultErrorText(reason)));
              }}
            >
              <Trash2 />
              {t.vaultForgetButton}
            </Button>
          </SettingsRow>
        </SettingsGroup>

        {error && <Notice>{error}</Notice>}
        {notice && (
          <p role="status" className="text-callout text-label-2">
            {notice}
          </p>
        )}
      </div>
      {screen === "shortcuts" && <ShortcutList onClose={() => setScreen("main")} only={MOBILE_SHORTCUTS} />}
    </div>
  );
}

/** The shortcuts that mean something in the touch app when a hardware keyboard is attached. */
export const MOBILE_SHORTCUTS = ["search-hosts", "shortcut-list", "settings", "go-hosts", "new-host", "close-tab", "copy", "paste"] as const;

/** Asks for the vault passphrase once before biometrics are turned on. */
function PassphraseConfirm({
  onCancel,
  onSubmit,
}: {
  onCancel: () => void;
  onSubmit: (passphrase: string) => Promise<void>;
}) {
  const [passphrase, setPassphrase] = useState("");
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (passphrase !== "")
      void onSubmit(passphrase).then(() => setPassphrase(""));
  };
  return (
    <form className="flex flex-col gap-3 px-4 py-3" onSubmit={submit}>
      <Input
        type="password"
        autoComplete="off"
        autoCapitalize="off"
        autoCorrect="off"
        spellCheck={false}
        value={passphrase}
        onChange={(event) => setPassphrase(event.target.value)}
        aria-label={t.vaultCurrentPassphrase}
        placeholder={t.vaultCurrentPassphrase}
        className="h-11"
      />
      <p className="text-callout text-label-3">{t.unlockEnableHint}</p>
      <Button
        type="submit"
        variant="primary"
        size="lg"
        disabled={passphrase === ""}
      >
        <Fingerprint />
        {t.unlockEnable}
      </Button>
      <Button type="button" variant="quiet" size="lg" onClick={onCancel}>
        {t.cancel}
      </Button>
    </form>
  );
}

/** Set, change or remove the vault passphrase. Typed text lives only in this
 *  component's state until submitted. */
function PassphraseForm({
  mode,
  onCancel,
  onSubmit,
}: {
  mode: "set" | "change" | "remove";
  onCancel: () => void;
  onSubmit: (current: string, next: string) => Promise<void>;
}) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const needsCurrent = mode !== "set";
  const needsNew = mode !== "remove";
  const problem = needsNew ? passphraseProblem(next, again) : null;
  const ready =
    (!needsCurrent || current !== "") &&
    (!needsNew || passphraseReady(next, again));

  const field = (
    label: string,
    value: string,
    set: (value: string) => void,
  ) => (
    <Input
      type="password"
      autoComplete="off"
      autoCapitalize="off"
      autoCorrect="off"
      spellCheck={false}
      value={value}
      onChange={(event) => set(event.target.value)}
      aria-label={label}
      placeholder={label}
      className="h-11"
    />
  );

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (ready)
      void onSubmit(current, next).then(
        () => (setCurrent(""), setNext(""), setAgain("")),
      );
  };

  return (
    <form className="flex flex-col gap-3 px-4 py-3" onSubmit={submit}>
      {needsCurrent && field(t.vaultCurrentPassphrase, current, setCurrent)}
      {needsNew && field(t.vaultNewPassphrase, next, setNext)}
      {needsNew && field(t.vaultRepeatPassphrase, again, setAgain)}
      {problem && <Notice>{problem}</Notice>}
      {needsNew && (
        <p className="text-callout text-label-3">{t.vaultPassphraseWarning}</p>
      )}
      <Button
        type="submit"
        variant={mode === "remove" ? "danger" : "primary"}
        size="lg"
        disabled={!ready}
      >
        {mode === "remove"
          ? t.vaultRemovePassphrase
          : mode === "change"
            ? t.vaultChangePassphrase
            : t.vaultSetPassphrase}
      </Button>
      <Button type="button" variant="quiet" size="lg" onClick={onCancel}>
        {t.cancel}
      </Button>
    </form>
  );
}
