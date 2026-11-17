import { Ban, CircleAlert, LoaderCircle, LogIn, LogOut, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tooltip } from "@/components/ui/tooltip";
import { t } from "@/lib/i18n";
import {
  syncDevices,
  syncNow,
  syncRevokeDevice,
  syncSignin,
  syncSignout,
  syncSignup,
  type SyncDevice,
} from "@/lib/syncApi";
import { statusText, syncErrorText, urlErrorText, useSyncStatus } from "@/lib/syncText";
import { checkServerUrl, fingerprintGroups, statusView, type Tone } from "@/lib/syncUi";
import { cn } from "@/lib/utils";

type Busy = "signin" | "signup" | "now" | "signout" | null;

const TONE: Record<Tone, string> = {
  neutral: "text-label-2",
  ok: "text-label-2",
  busy: "text-label-2",
  warn: "text-warning",
  error: "text-danger",
};

/** Settings → Sync: the account on your own server, this device's status,
 *  the devices on the account and "Sync now". */
export function SyncSettings() {
  const { status, error: statusError, refresh } = useSyncStatus();
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState({ server: "", username: "", passphrase: "", device: "" });
  const [confirmSignOut, setConfirmSignOut] = useState(false);

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
    if (!check.ok) {
      setError(urlErrorText(check));
      return;
    }
    const device = form.device.trim() || undefined;
    const call = kind === "signin" ? syncSignin : syncSignup;
    void run(kind, () => call(check.url, form.username.trim(), form.passphrase, device));
  };

  if (!status) {
    return <p className="px-3 py-2 text-caption font-normal text-label-3">{statusError ?? "…"}</p>;
  }

  const view = statusView(status);

  if (!status.signed_in) {
    const ready = form.server.trim() && form.username.trim() && form.passphrase;
    return (
      <form
        className="space-y-2 px-3 py-3"
        onSubmit={(event) => {
          event.preventDefault();
          submit("signin");
        }}
      >
        <p className="text-caption font-normal text-label-2">{t.syncIntro}</p>
        <Input
          value={form.server}
          onChange={(event) => setForm({ ...form, server: event.target.value })}
          placeholder={t.syncServerPlaceholder}
          aria-label={t.syncServer}
          autoComplete="off"
          spellCheck={false}
          className="h-8 font-mono text-caption font-normal"
        />
        <Input
          value={form.username}
          onChange={(event) => setForm({ ...form, username: event.target.value })}
          placeholder={t.syncAccountName}
          aria-label={t.syncAccountName}
          autoComplete="off"
          spellCheck={false}
          className="h-8 text-caption font-normal"
        />
        <Input
          type="password"
          value={form.passphrase}
          onChange={(event) => setForm({ ...form, passphrase: event.target.value })}
          placeholder={t.syncAccountPassphrase}
          aria-label={t.syncAccountPassphrase}
          autoComplete="off"
          className="h-8 text-caption font-normal"
        />
        <Input
          value={form.device}
          onChange={(event) => setForm({ ...form, device: event.target.value })}
          placeholder={t.syncDeviceName}
          aria-label={t.syncDeviceName}
          autoComplete="off"
          className="h-8 text-caption font-normal"
        />
        <p className="text-caption font-normal text-label-3">{t.syncPassphraseHint}</p>
        <Message text={error ?? statusError} />
        <div className="flex justify-end gap-1.5">
          <Button
            type="button"
            variant="secondary"
            size="sm"
            disabled={!ready || busy !== null}
            onClick={() => submit("signup")}
          >
            {busy === "signup" && <Spinner />}
            {t.syncSignUp}
          </Button>
          <Button type="submit" variant="primary" size="sm" disabled={!ready || busy !== null}>
            {busy === "signin" ? <Spinner /> : <LogIn />}
            {t.syncSignIn}
          </Button>
        </div>
      </form>
    );
  }

  const groups = status.fingerprint ? fingerprintGroups(status.fingerprint) : [];

  return (
    <>
      <Row label={t.syncSignedInAs}>
        <span className="truncate text-caption font-normal text-label-2">{status.username}</span>
      </Row>
      <Row label={t.syncServerRow}>
        <span className="truncate font-mono text-caption font-normal text-label-2" title={status.server_url ?? ""}>
          {status.server_url}
        </span>
      </Row>
      <div className="flex items-start gap-2 px-3 py-2.5">
        <span className="shrink-0 text-callout text-label-1">{t.syncStatusRow}</span>
        <p role="status" className={cn("min-w-0 flex-1 text-right text-caption font-normal break-words", TONE[view.tone])}>
          {view.tone === "busy" && <LoaderCircle className="mr-1 inline size-3.5 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />}
          <span className="selectable">{statusText(view)}</span>
        </p>
      </div>
      <div className="flex items-center justify-end gap-1.5 px-3 py-2">
        {confirmSignOut ? (
          <>
            <span className="mr-auto text-caption font-normal text-warning">{t.syncSignOutHint}</span>
            <Button variant="ghost" size="sm" onClick={() => setConfirmSignOut(false)}>
              {t.cancel}
            </Button>
            <Button
              variant="destructive"
              size="sm"
              onClick={() => {
                setConfirmSignOut(false);
                void run("signout", syncSignout);
              }}
            >
              {t.syncSignOutConfirm}
            </Button>
          </>
        ) : (
          <>
            <Button variant="secondary" size="sm" disabled={busy !== null} onClick={() => setConfirmSignOut(true)}>
              <LogOut />
              {t.syncSignOut}
            </Button>
            <Button
              variant="primary"
              size="sm"
              disabled={busy !== null || status.state === "syncing"}
              onClick={() => void run("now", syncNow)}
            >
              {busy === "now" ? <Spinner /> : <RefreshCw />}
              {t.syncNow}
            </Button>
          </>
        )}
      </div>
      <Message text={error ?? statusError} />
      {groups.length > 0 && (
        <div className="px-3 py-2.5">
          <p className="text-callout text-label-1">{t.syncSafetyNumber}</p>
          <p className="mt-0.5 text-caption font-normal text-label-2">{t.syncSafetyHint}</p>
          <SafetyNumber groups={groups} />
        </div>
      )}
      <Devices signedIn={status.signed_in} onError={setError} />
    </>
  );
}

/** The safety number as a grid of digit groups, easy to read aloud. */
export function SafetyNumber({ groups }: { groups: string[] }) {
  return (
    <ul
      aria-label={groups.join(" ")}
      className="selectable mt-1.5 grid grid-cols-3 gap-x-3 gap-y-1 rounded-sm bg-fill-1 px-3 py-2 font-mono text-caption font-normal tabular-nums text-label-1"
    >
      {groups.map((group, index) => (
        <li key={index} aria-hidden="true">
          {group}
        </li>
      ))}
    </ul>
  );
}

function Devices({ signedIn, onError }: { signedIn: boolean; onError: (text: string | null) => void }) {
  const [devices, setDevices] = useState<SyncDevice[]>([]);
  const [confirming, setConfirming] = useState<string | null>(null);

  const reload = useCallback(() => {
    if (!signedIn) return;
    syncDevices()
      .then(setDevices)
      .catch((reason) => onError(syncErrorText(reason)));
  }, [signedIn, onError]);
  useEffect(reload, [reload]);

  const revoke = async (id: string) => {
    setConfirming(null);
    try {
      await syncRevokeDevice(id);
      onError(null);
    } catch (reason) {
      onError(syncErrorText(reason));
    }
    reload();
  };

  return (
    <div className="px-3 py-2.5">
      <p className="text-callout text-label-1">{t.syncDevices}</p>
      <ul className="mt-1 space-y-1">
        {devices.map((device) => (
          <li key={device.device_id} className="flex min-h-8 items-center gap-2">
            <div className="min-w-0 flex-1">
              <p className={cn("truncate text-caption", device.revoked ? "text-label-3 line-through" : "text-label-1")}>
                {device.name}
              </p>
              <p className="text-caption font-normal text-label-3">
                {device.this_device
                  ? t.syncThisDevice
                  : device.revoked
                    ? device.revoked_at
                      ? `${t.syncRevoked} · ${new Date(device.revoked_at * 1000).toLocaleDateString()}`
                      : t.syncRevoked
                    : new Date(device.created_at * 1000).toLocaleDateString()}
              </p>
            </div>
            {!device.this_device && !device.revoked &&
              (confirming === device.device_id ? (
                <>
                  <Button variant="ghost" size="sm" onClick={() => setConfirming(null)}>
                    {t.cancel}
                  </Button>
                  <Button variant="destructive" size="sm" onClick={() => void revoke(device.device_id)}>
                    {t.syncRevokeConfirm}
                  </Button>
                </>
              ) : (
                <Tooltip label={`${t.syncRevoke}: ${device.name}`}>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={`${t.syncRevoke}: ${device.name}`}
                    onClick={() => setConfirming(device.device_id)}
                  >
                    <Ban />
                  </Button>
                </Tooltip>
              ))}
          </li>
        ))}
      </ul>
      <p className="mt-1 text-caption font-normal text-label-3">{t.syncRevokeHint}</p>
    </div>
  );
}

function Message({ text }: { text: string | null }) {
  if (!text) return null;
  return (
    <p role="alert" className="flex items-start gap-2 px-3 py-2 text-caption font-normal break-words text-danger">
      <CircleAlert className="mt-px size-3.5 shrink-0" strokeWidth={1.5} />
      <span className="selectable min-w-0">{text}</span>
    </p>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-h-11 items-center gap-3 px-3 py-1.5">
      <span className="shrink-0 text-callout text-label-1">{label}</span>
      <div className="flex min-w-0 flex-1 justify-end">{children}</div>
    </div>
  );
}

function Spinner() {
  return <LoaderCircle className="animate-spin motion-reduce:animate-none" />;
}
