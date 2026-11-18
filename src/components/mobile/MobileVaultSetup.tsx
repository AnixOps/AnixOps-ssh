import { ShieldCheck } from "lucide-react";
import { useState, type FormEvent } from "react";
import { MobileSwitch } from "@/components/mobile/MobileSwitch";
import { Button } from "@/components/ui/button";
import { Field, Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import {
  vaultEnableUnlock,
  vaultSetPassphrase,
  vaultSetupSkip,
  type VaultStatus,
} from "@/lib/backend";
import { t } from "@/lib/i18n";
import {
  applySetup,
  biometricMethod,
  passphraseProblem,
  passphraseReady,
  type SetupChoice,
} from "@/lib/mobileVault";
import { vaultErrorText } from "@/lib/vaultUi";

/** The first run: how to protect saved passwords and keys. A passphrase (with
 *  biometrics on top, when the phone has them) is the way a vault is meant to
 *  be used; "this phone only" skips it and relies on the secure hardware that
 *  already holds the key. The choice can be changed later in Settings. */
export function MobileVaultSetup({
  status,
  onDone,
}: {
  status: VaultStatus;
  onDone: (status: VaultStatus) => void;
}) {
  const biometrics = biometricMethod(status);
  const [passphrase, setPassphrase] = useState("");
  const [again, setAgain] = useState("");
  const [useBiometrics, setUseBiometrics] = useState(biometrics !== undefined);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** The passphrase is set but biometrics could not be turned on: say so before leaving. */
  const [partial, setPartial] = useState<VaultStatus | null>(null);
  const problem = passphraseProblem(passphrase, again);
  const ready = passphraseReady(passphrase, again);

  const run = async (choice: SetupChoice) => {
    setBusy(true);
    setError(null);
    try {
      const result = await applySetup(choice, {
        setPassphrase: (value) => vaultSetPassphrase(value),
        enableUnlock: vaultEnableUnlock,
        skip: vaultSetupSkip,
      });
      if (result.biometricsFailed) setPartial(result.status);
      else onDone(result.status);
    } catch (reason) {
      setError(vaultErrorText(reason));
    } finally {
      setBusy(false);
    }
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (ready && !busy)
      void run({
        kind: "passphrase",
        passphrase,
        biometrics: useBiometrics && biometrics !== undefined,
      });
  };

  return (
    <form
      onSubmit={submit}
      aria-labelledby="mobile-setup-title"
      className="fixed inset-0 z-modal flex flex-col bg-bg"
    >
      <div className="px-safe pt-safe flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto py-6">
        <div className="flex flex-col gap-2">
          <ShieldCheck
            className="size-8 text-(--accent)"
            strokeWidth={1.5}
            aria-hidden
          />
          <h1 id="mobile-setup-title" className="text-title-1">
            {t.mvSetupTitle}
          </h1>
          <p className="text-callout text-label-2">{t.mvSetupBody}</p>
        </div>

        {partial ? (
          <Notice tone="warning">{t.mvSetupBiometricFailed}</Notice>
        ) : (
          <>
            <Field label={t.vaultNewPassphrase}>
              <Input
                type="password"
                autoComplete="off"
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                value={passphrase}
                onChange={(event) => setPassphrase(event.target.value)}
                className="h-11"
              />
            </Field>
            <Field label={t.vaultRepeatPassphrase}>
              <Input
                type="password"
                autoComplete="off"
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                value={again}
                onChange={(event) => setAgain(event.target.value)}
                className="h-11"
              />
            </Field>
            {problem && <Notice>{problem}</Notice>}
            <p className="text-callout text-label-3">
              {t.vaultPassphraseWarning}
            </p>

            {biometrics ? (
              <div className="flex min-h-11 items-center gap-3">
                <div className="min-w-0 flex-1">
                  <p className="text-body text-label-1">{t.mvSetupBiometric}</p>
                  <p className="text-callout text-label-2">
                    {t.mvSetupBiometricHint}
                  </p>
                </div>
                <MobileSwitch
                  on={useBiometrics}
                  label={t.mvSetupBiometric}
                  onClick={() => setUseBiometrics(!useBiometrics)}
                />
              </div>
            ) : (
              <p className="text-callout text-label-3">
                {t.mvSetupBiometricMissing}
              </p>
            )}
            {error && <Notice>{error}</Notice>}
          </>
        )}
      </div>

      <div className="px-safe pb-safe material-bar flex shrink-0 flex-col gap-2 border-t py-3">
        {partial ? (
          <Button
            type="button"
            variant="primary"
            size="lg"
            onClick={() => onDone(partial)}
          >
            {t.mvContinue}
          </Button>
        ) : (
          <>
            <Button
              type="submit"
              variant="primary"
              size="lg"
              disabled={!ready || busy}
            >
              {t.mvSetupFinish}
            </Button>
            <p className="text-center text-callout text-label-3">
              {t.mvSetupSkipHint}
            </p>
            <Button
              type="button"
              variant="quiet"
              size="lg"
              disabled={busy}
              onClick={() => void run({ kind: "device" })}
            >
              {t.mvSetupSkip}
            </Button>
          </>
        )}
      </div>
    </form>
  );
}
