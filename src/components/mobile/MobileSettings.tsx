import {
  ArrowLeft,
  Fingerprint,
  Lock,
  LockOpen,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import { useState, type FormEvent } from "react";
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
import { t } from "@/lib/i18n";
import {
  BIOMETRIC,
  backgroundLockChoices,
  backgroundLockLabel,
  biometricMethod,
  passphraseProblem,
  passphraseReady,
} from "@/lib/mobileVault";
import { vaultErrorText } from "@/lib/vaultUi";

type Editing = "set" | "change" | "remove" | "biometric" | null;

/** The phone's settings: today the vault only. A full screen over the host
 *  list with Back on the left. Forms open under their row with the actions
 *  stacked at full width (actions.md O5). */
export function MobileSettings({
  status,
  onStatus,
  onBack,
}: {
  status: VaultStatus;
  onStatus: (status: VaultStatus) => void;
  onBack: () => void;
}) {
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

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="mobile-settings-title"
      className="fixed inset-0 z-drawer flex flex-col bg-bg"
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
    </div>
  );
}

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
