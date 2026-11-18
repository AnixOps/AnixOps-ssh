import { Ban, LoaderCircle, LogIn, LogOut, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { SafetyNumber } from "@/components/app/SyncSettings";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Field, Notice } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { t } from "@/lib/i18n";
import { syncDevices, syncNow, syncRevokeDevice, syncSignin, syncSignout, syncSignup, type SyncDevice } from "@/lib/syncApi";
import { statusText, syncErrorText, urlErrorText, useSyncStatus } from "@/lib/syncText";
import { checkServerUrl, fingerprintGroups, statusView } from "@/lib/syncUi";
import { cn } from "@/lib/utils";

type Busy = "signin" | "signup" | "now" | "signout" | null;

/** Sync on the phone: sign in to your own server, see the state, sync now,
 *  manage the devices. Team vaults and host history are on the desktop. The
 *  calls are the desktop's (`sync_call`), answered by the engine in-process. */
export function MobileSync() {
  const { status, error: statusError, refresh } = useSyncStatus();
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState({ server: "", username: "", passphrase: "", device: "" });
  const confirm = useConfirm();

  const run = async (kind: Exclude<Busy, null>, action: () => Promise<unknown>) => {
    setBusy(kind);
    setError(null);
    try {
      await action();
      setForm((current) => ({ ...current, passphrase: "" }));
    } catch (reason) {
      setError(syncErrorText(reason));
    } finally {
      setBusy(null);
      refresh();
    }
  };

  const submit = (kind: "signin" | "signup") => {
    const check = checkServerUrl(form.server);
    if (!check.ok) return setError(urlErrorText(check));
    const device = form.device.trim() || undefined;
    const call = kind === "signin" ? syncSignin : syncSignup;
    void run(kind, () => call(check.url, form.username.trim(), form.passphrase, device));
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="material-bar pt-safe shrink-0 border-b">
        <h1 className="px-safe flex h-14 items-center truncate text-title-3">{t.syncTitle}</h1>
      </header>
      {!status ? (
        <p className="px-safe py-4 text-callout text-label-3">{statusError ?? "…"}</p>
      ) : status.signed_in ? (
        <SignedIn
          status={status}
          busy={busy}
          error={error ?? statusError}
          onNow={() => void run("now", syncNow)}
          onSignOut={async () => {
            const ok = await confirm({
              title: t.syncSignOutTitle,
              body: t.syncSignOutHint,
              verb: t.syncSignOut,
              destructive: false,
            });
            if (ok) void run("signout", syncSignout);
          }}
          onError={setError}
        />
      ) : (
        <form
          className="flex min-h-0 flex-1 flex-col"
          onSubmit={(event) => {
            event.preventDefault();
            submit("signin");
          }}
        >
          <div className="px-safe flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto py-5">
            <p className="text-callout text-label-2">{t.syncIntro}</p>
            {(error ?? statusError) && <Notice>{error ?? statusError}</Notice>}
            <Field label={t.syncServer}>
              <Input
                className="h-11 font-mono"
                value={form.server}
                onChange={(event) => setForm({ ...form, server: event.target.value })}
                placeholder={t.syncServerPlaceholder}
                inputMode="url"
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
              />
            </Field>
            <Field label={t.syncAccountName}>
              <Input
                className="h-11"
                value={form.username}
                onChange={(event) => setForm({ ...form, username: event.target.value })}
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
              />
            </Field>
            <Field label={t.syncAccountPassphrase}>
              <Input
                className="h-11"
                type="password"
                value={form.passphrase}
                onChange={(event) => setForm({ ...form, passphrase: event.target.value })}
                autoComplete="off"
              />
            </Field>
            <Field label={t.syncDeviceName} hint={t.optional}>
              <Input
                className="h-11"
                value={form.device}
                onChange={(event) => setForm({ ...form, device: event.target.value })}
                autoCorrect="off"
              />
            </Field>
            <p className="text-callout text-label-3">{t.syncPassphraseHint}</p>
            <p className="text-callout text-label-3">{t.mobileSyncSessionHint}</p>
          </div>
          <div className="px-safe material-bar flex shrink-0 flex-col gap-2 border-t py-3">
            <Button
              type="submit"
              variant="primary"
              size="lg"
              disabled={!(form.server.trim() && form.username.trim() && form.passphrase) || busy !== null}
            >
              {busy === "signin" ? <Spinner /> : <LogIn />}
              {t.syncSignIn}
            </Button>
            <Button
              type="button"
              variant="secondary"
              size="lg"
              disabled={!(form.server.trim() && form.username.trim() && form.passphrase) || busy !== null}
              onClick={() => submit("signup")}
            >
              {busy === "signup" && <Spinner />}
              {t.syncSignUp}
            </Button>
          </div>
        </form>
      )}
    </div>
  );
}

function SignedIn({
  status,
  busy,
  error,
  onNow,
  onSignOut,
  onError,
}: {
  status: NonNullable<ReturnType<typeof useSyncStatus>["status"]>;
  busy: Busy;
  error: string | null;
  onNow: () => void;
  onSignOut: () => void;
  onError: (text: string | null) => void;
}) {
  const view = statusView(status);
  const groups = status.fingerprint ? fingerprintGroups(status.fingerprint) : [];
  return (
    <>
      <div className="px-safe flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto py-5">
        {error && <Notice>{error}</Notice>}
        <dl className="divide-y rounded-md bg-bg-grouped">
          <Row label={t.syncSignedInAs} value={status.username ?? ""} />
          <Row label={t.syncServerRow} value={status.server_url ?? ""} mono />
          <div className="flex items-start gap-3 px-4 py-3">
            <dt className="shrink-0 text-callout text-label-1">{t.syncStatusRow}</dt>
            <dd
              role="status"
              className={cn(
                "min-w-0 flex-1 text-right text-callout break-words",
                view.tone === "error" ? "text-danger" : view.tone === "warn" ? "text-warning" : "text-label-2",
              )}
            >
              {view.tone === "busy" && (
                <LoaderCircle className="mr-1 inline size-3.5 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
              )}
              <span className="selectable">{statusText(view)}</span>
            </dd>
          </div>
        </dl>
        {groups.length > 0 && (
          <div>
            <p className="text-callout text-label-1">{t.syncSafetyNumber}</p>
            <p className="mt-0.5 text-callout text-label-2">{t.syncSafetyHint}</p>
            <SafetyNumber groups={groups} />
          </div>
        )}
        <Devices onError={onError} />
      </div>
      <div className="px-safe material-bar flex shrink-0 flex-col gap-2 border-t py-3">
        <Button variant="primary" size="lg" disabled={busy !== null || status.state === "syncing"} onClick={onNow}>
          {busy === "now" ? <Spinner /> : <RefreshCw />}
          {t.syncNow}
        </Button>
        <Button variant="secondary" size="lg" disabled={busy !== null} onClick={onSignOut}>
          <LogOut />
          {t.syncSignOut}
        </Button>
      </div>
    </>
  );
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex items-baseline gap-3 px-4 py-3">
      <dt className="shrink-0 text-callout text-label-1">{label}</dt>
      <dd className={cn("selectable min-w-0 flex-1 truncate text-right text-callout text-label-2", mono && "font-mono")}>
        {value}
      </dd>
    </div>
  );
}

function Devices({ onError }: { onError: (text: string | null) => void }) {
  const [devices, setDevices] = useState<SyncDevice[]>([]);
  const confirm = useConfirm();

  const reload = useCallback(() => {
    syncDevices()
      .then(setDevices)
      .catch((reason) => onError(syncErrorText(reason)));
  }, [onError]);
  useEffect(reload, [reload]);

  const revoke = async (device: SyncDevice) => {
    const title = t.syncRevokeTitle.replace("{name}", device.name);
    if (!(await confirm({ title, body: t.syncRevokeHint, verb: t.syncRevoke }))) return;
    try {
      await syncRevokeDevice(device.device_id);
      onError(null);
    } catch (reason) {
      onError(syncErrorText(reason));
    }
    reload();
  };

  return (
    <div>
      <p className="text-callout text-label-1">{t.syncDevices}</p>
      <ul className="mt-1 divide-y">
        {devices.map((device) => (
          <li key={device.device_id} className="flex min-h-14 items-center gap-2">
            <div className="min-w-0 flex-1">
              <p className={cn("truncate text-body", device.revoked ? "text-label-3 line-through" : "text-label-1")}>
                {device.name}
              </p>
              <p className="text-callout text-label-3">
                {device.this_device
                  ? t.syncThisDevice
                  : device.revoked
                    ? t.syncRevoked
                    : new Date(device.created_at * 1000).toLocaleDateString()}
              </p>
            </div>
            {!device.this_device && !device.revoked && (
              <Button
                variant="quiet"
                size="icon-lg"
                aria-label={`${t.syncRevoke}: ${device.name}`}
                onClick={() => void revoke(device)}
              >
                <Ban />
              </Button>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

function Spinner() {
  return <LoaderCircle className="animate-spin motion-reduce:animate-none" />;
}
