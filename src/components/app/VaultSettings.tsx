import { CircleAlert, Lock, LockOpen, ShieldCheck, Trash2 } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  onVaultChanged,
  vaultForgetAll,
  vaultLock,
  vaultRemovePassphrase,
  vaultSetAutoLock,
  vaultSetPassphrase,
  vaultStatus,
  type VaultStatus,
} from "@/lib/backend";
import { t } from "@/lib/i18n";
import { requestVaultUnlock, vaultErrorText } from "@/lib/vaultUi";
import { cn } from "@/lib/utils";

const AUTO_LOCK = [5, 15, 30, 60, 240, 0];
const MIN_PASSPHRASE = 8;

type Editing = "set" | "change" | "remove" | null;

/** Settings → Security: what protects saved passwords and keys, the vault
 *  passphrase, lock now, auto-lock and "forget everything". */
export function VaultSettings() {
  const [status, setStatus] = useState<VaultStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [editing, setEditing] = useState<Editing>(null);
  const [confirmForget, setConfirmForget] = useState(false);

  useEffect(() => {
    vaultStatus().then(setStatus).catch((reason) => setError(String(reason)));
    return onVaultChanged(() => void vaultStatus().then(setStatus).catch(console.error));
  }, []);

  const run = async (action: () => Promise<VaultStatus>) => {
    setError(null);
    setNotice(null);
    try {
      setStatus(await action());
      return true;
    } catch (reason) {
      setError(vaultErrorText(reason));
      return false;
    }
  };

  if (!status) {
    return <p className="px-3 py-2 text-caption font-normal text-label-3">{error ?? "…"}</p>;
  }

  const autoLockChoices = AUTO_LOCK.includes(status.autoLockMinutes)
    ? AUTO_LOCK
    : [...AUTO_LOCK, status.autoLockMinutes].sort((a, b) => (a || Infinity) - (b || Infinity));

  return (
    <>
      <div className="flex items-start gap-2 px-3 py-2.5">
        <ShieldCheck className="mt-0.5 size-4 shrink-0 text-(--accent)" strokeWidth={1.5} />
        <p className="text-caption font-normal text-label-2">
          {status.passphraseSet ? t.vaultIntroPassphrase : t.vaultIntroKeychain}
        </p>
      </div>
      {!status.keychainAvailable && (
        <p className="flex items-start gap-2 px-3 py-2 text-caption font-normal text-warning">
          <CircleAlert className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} />
          <span>
            {t.vaultKeychainMissing}
            {status.keychainError ? `: ${status.keychainError}` : ""}
          </span>
        </p>
      )}

      <VaultRow label={t.vaultState}>
        <span
          className={cn("flex items-center gap-1.5 text-caption font-normal", status.unlocked ? "text-label-2" : "text-label-3")}
        >
          {status.unlocked ? <LockOpen className="size-3.5" strokeWidth={1.5} /> : <Lock className="size-3.5" strokeWidth={1.5} />}
          {status.unlocked ? t.vaultUnlocked : t.vaultLocked}
        </span>
        {status.unlocked ? (
          <Button variant="secondary" size="sm" onClick={() => void run(vaultLock)}>
            <Lock />
            {t.vaultLockNow}
          </Button>
        ) : (
          <Button variant="secondary" size="sm" onClick={requestVaultUnlock} disabled={!status.passphraseSet}>
            <LockOpen />
            {t.vaultUnlock}
          </Button>
        )}
      </VaultRow>

      <VaultRow label={t.vaultPassphrase} hint={status.passphraseSet ? t.vaultPassphraseSet : t.vaultPassphraseUnset}>
        {status.passphraseSet ? (
          <>
            <Button variant="secondary" size="sm" onClick={() => setEditing(editing === "change" ? null : "change")}>
              {t.vaultChangePassphrase}
            </Button>
            <Button variant="ghost" size="sm" onClick={() => setEditing(editing === "remove" ? null : "remove")}>
              {t.vaultRemovePassphrase}
            </Button>
          </>
        ) : (
          <Button
            variant="secondary"
            size="sm"
            disabled={!status.keychainAvailable}
            onClick={() => setEditing(editing === "set" ? null : "set")}
          >
            {t.vaultSetPassphrase}
          </Button>
        )}
      </VaultRow>
      {editing && (
        <PassphraseForm
          mode={editing}
          onCancel={() => setEditing(null)}
          onSubmit={async (current, next) => {
            const done = await run(() =>
              editing === "remove"
                ? vaultRemovePassphrase(current)
                : vaultSetPassphrase(next, editing === "change" ? current : null),
            );
            if (done) setEditing(null);
          }}
        />
      )}

      <VaultRow label={t.vaultAutoLock} hint={t.vaultAutoLockHint}>
        <select
          aria-label={t.vaultAutoLock}
          value={status.autoLockMinutes}
          onChange={(event) => void run(() => vaultSetAutoLock(Number(event.target.value)))}
          className="h-7 rounded-xs bg-fill-1 px-2 text-callout text-label-1 outline-none focus-visible:ring-2 focus-visible:ring-(--accent)"
        >
          {autoLockChoices.map((minutes) => (
            <option key={minutes} value={minutes}>
              {minutes === 0 ? t.vaultNever : `${minutes} ${t.vaultMinutes}`}
            </option>
          ))}
        </select>
      </VaultRow>

      <div className="space-y-2 px-3 py-2.5">
        <p className="text-caption font-normal text-label-3">{t.vaultForgetHint}</p>
        <Button
          variant={confirmForget ? "destructive" : "secondary"}
          size="sm"
          onClick={() => {
            if (!confirmForget) {
              setConfirmForget(true);
              return;
            }
            setConfirmForget(false);
            setError(null);
            vaultForgetAll()
              .then((hosts) => setNotice(`${t.vaultForgotten}: ${hosts}`))
              .catch((reason) => setError(String(reason)));
          }}
          onBlur={() => setConfirmForget(false)}
        >
          <Trash2 />
          {confirmForget ? t.vaultForgetConfirm : t.vaultForgetAll}
        </Button>
      </div>

      {error && (
        <p role="alert" className="px-3 py-2 text-caption font-normal text-danger">
          {error}
        </p>
      )}
      {notice && <p className="px-3 py-2 text-caption font-normal text-label-2">{notice}</p>}
    </>
  );
}

function VaultRow({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="flex min-h-11 flex-wrap items-center gap-x-3 gap-y-1.5 px-3 py-1.5">
      <div className="min-w-0 flex-1 basis-32">
        <p className="text-callout text-label-1">{label}</p>
        {hint && <p className="text-caption font-normal text-label-2">{hint}</p>}
      </div>
      <div className="flex items-center gap-2">{children}</div>
    </div>
  );
}

/** Set, change or remove the vault passphrase. The text typed stays in this
 *  component's state until submitted and is never stored. */
function PassphraseForm({
  mode,
  onCancel,
  onSubmit,
}: {
  mode: Exclude<Editing, null>;
  onCancel: () => void;
  onSubmit: (current: string, next: string) => Promise<void>;
}) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const needsCurrent = mode !== "set";
  const needsNew = mode !== "remove";
  const problem = needsNew && next !== "" ? (next.length < MIN_PASSPHRASE ? t.vaultPassphraseShort : next !== again ? t.vaultPassphraseMismatch : null) : null;
  const ready = (!needsCurrent || current !== "") && (!needsNew || (next.length >= MIN_PASSPHRASE && next === again));

  const field = (label: string, value: string, set: (value: string) => void) => (
    <Input
      type="password"
      autoComplete="off"
      value={value}
      onChange={(event) => set(event.target.value)}
      aria-label={label}
      placeholder={label}
    />
  );

  return (
    <form
      className="space-y-2 px-3 py-2.5"
      onSubmit={(event) => {
        event.preventDefault();
        if (ready) void onSubmit(current, next).then(() => (setCurrent(""), setNext(""), setAgain("")));
      }}
    >
      {needsCurrent && field(t.vaultCurrentPassphrase, current, setCurrent)}
      {needsNew && field(t.vaultNewPassphrase, next, setNext)}
      {needsNew && field(t.vaultRepeatPassphrase, again, setAgain)}
      {problem && <p className="text-caption font-normal text-danger">{problem}</p>}
      {needsNew && <p className="text-caption font-normal text-label-3">{t.vaultPassphraseWarning}</p>}
      <div className="flex justify-end gap-2">
        <Button type="button" variant="secondary" size="sm" onClick={onCancel}>
          {t.cancel}
        </Button>
        <Button type="submit" variant="primary" size="sm" disabled={!ready}>
          {mode === "remove" ? t.vaultRemovePassphrase : mode === "change" ? t.vaultChangePassphrase : t.vaultSetPassphrase}
        </Button>
      </div>
    </form>
  );
}
