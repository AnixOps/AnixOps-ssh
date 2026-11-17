import { LockKeyhole } from "lucide-react";
import { AnimatePresence } from "motion/react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Overlay } from "@/components/ui/overlay";
import { vaultUnlock } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { onVaultUnlockRequested, vaultErrorText } from "@/lib/vaultUi";

/** Asks for the vault passphrase when something needs a saved secret while
 *  the vault is locked (a terminal that uses a saved key, a failed save). */
export function VaultUnlockDialog() {
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => onVaultUnlockRequested(() => setOpen(true)), []);
  useEffect(() => {
    if (open) window.requestAnimationFrame(() => inputRef.current?.focus());
  }, [open]);

  const close = () => {
    setOpen(false);
    setValue("");
    setError(null);
  };

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await vaultUnlock(value);
      close();
    } catch (reason) {
      setError(vaultErrorText(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <AnimatePresence>
      {open && (
        <Overlay labelledBy="vault-unlock-title" onDismiss={close} onSubmit={() => void submit()} className="max-w-md p-6">
          <div className="flex items-start gap-3">
            <LockKeyhole className="mt-0.5 size-6 shrink-0 text-(--accent)" strokeWidth={1.5} />
            <div className="min-w-0 flex-1">
              <h2 id="vault-unlock-title" className="text-title-3">
                {t.vaultUnlockTitle}
              </h2>
              <p className="mt-2 text-callout text-label-2">{t.vaultUnlockHint}</p>
            </div>
          </div>
          <Input
            ref={inputRef}
            type="password"
            autoComplete="off"
            value={value}
            onChange={(event) => setValue(event.target.value)}
            aria-label={t.vaultPassphrase}
            className="mt-5"
          />
          {error && (
            <Notice className="mt-2">{error}</Notice>
          )}
          <div className="mt-6 flex justify-end gap-2">
            <Button type="button" variant="secondary" onClick={close}>
              {t.cancel}
            </Button>
            <Button type="submit" variant="primary" disabled={busy || value === ""}>
              {t.vaultUnlock}
            </Button>
          </div>
        </Overlay>
      )}
    </AnimatePresence>
  );
}
