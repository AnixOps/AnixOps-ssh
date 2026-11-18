import { Fingerprint } from "lucide-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import mark from "@/assets/mark.svg";
import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { vaultUnlock, vaultUnlockWith, type VaultStatus } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { BIOMETRIC, biometricReady } from "@/lib/mobileVault";
import { vaultErrorText } from "@/lib/vaultUi";

/** Shown over everything while a passphrase vault is locked, which on a phone
 *  is every time the app comes back from the background (the default lock is
 *  immediate). Biometrics, when turned on, are tried once on arrival; the
 *  passphrase field is always there, so a cancelled or failed prompt is never
 *  a dead end. The page behind is made inert by the caller. */
export function MobileLockScreen({
  status,
  onUnlocked,
}: {
  status: VaultStatus;
  onUnlocked: (status: VaultStatus) => void;
}) {
  const [passphrase, setPassphrase] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const triedBiometrics = useRef(false);
  const biometrics = biometricReady(status);

  const attempt = async (action: () => Promise<VaultStatus>) => {
    setBusy(true);
    setError(null);
    try {
      onUnlocked(await action());
    } catch (reason) {
      setError(vaultErrorText(reason));
      setBusy(false);
    }
  };

  const withBiometrics = () => void attempt(() => vaultUnlockWith(BIOMETRIC));

  // Ask for the sensor on arrival, once per lock. Cancelling it falls back to
  // the field below, which has the focus by then.
  useEffect(() => {
    if (biometrics && !triedBiometrics.current) {
      triedBiometrics.current = true;
      withBiometrics();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [biometrics]);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (passphrase !== "" && !busy) void attempt(() => vaultUnlock(passphrase));
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="mobile-lock-title"
      className="fixed inset-0 z-modal flex flex-col bg-bg pt-safe pb-safe"
    >
      <form
        onSubmit={submit}
        className="px-safe flex min-h-0 flex-1 flex-col justify-center gap-5 overflow-y-auto py-8"
      >
        <div className="flex flex-col items-center gap-3 text-center">
          <img src={mark} alt="" className="size-16" draggable={false} />
          <h1 id="mobile-lock-title" className="text-title-2">
            {t.mvLockedTitle}
          </h1>
          <p className="max-w-xs text-callout text-label-2">{t.mvLockedHint}</p>
        </div>
        <div className="mx-auto flex w-full max-w-sm flex-col gap-3">
          <Input
            ref={inputRef}
            type="password"
            autoComplete="off"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={passphrase}
            onChange={(event) => setPassphrase(event.target.value)}
            aria-label={t.vaultPassphrase}
            placeholder={t.vaultPassphrase}
            className="h-11"
          />
          {error && <Notice>{error}</Notice>}
          <Button
            type="submit"
            variant="primary"
            size="lg"
            disabled={busy || passphrase === ""}
          >
            {t.vaultUnlock}
          </Button>
          {biometrics && (
            <Button
              type="button"
              variant="secondary"
              size="lg"
              disabled={busy}
              onClick={withBiometrics}
            >
              <Fingerprint />
              {t.unlockWithBiometric}
            </Button>
          )}
        </div>
      </form>
    </div>
  );
}
