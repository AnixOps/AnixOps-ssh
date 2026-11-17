import { Ban, LoaderCircle, LogIn, LogOut, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm";
import { Notice } from "@/components/ui/form";
import { SettingsRow } from "@/components/ui/settings";
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
    if (!check.ok) {
      setError(urlErrorText(check));
      return;
    }
    const device = form.device.trim() || undefined;
    const call = kind === "signin" ? syncSignin : syncSignup;
    void run(kind, () => call(check.url, form.username.trim(), form.passphrase, device));
  };

  if (!status) {
    return <p className="px-3 py-2 text-callout text-label-3">{statusError ?? "…"}</p>;
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
        <p className="text-callout text-label-2">{t.syncIntro}</p>
        <Input
          value={form.server}
          onChange={(event) => setForm({ ...form, server: event.target.value })}
          placeholder={t.syncServerPlaceholder}
          aria-label={t.syncServer}
          autoComplete="off"
          spellCheck={false}
          className="h-8 font-mono text-callout"
        />
        <Input
          value={form.username}
          onChange={(event) => setForm({ ...form, username: event.target.value })}
          placeholder={t.syncAccountName}
          aria-label={t.syncAccountName}
          autoComplete="off"
          spellCheck={false}
          className="h-8 text-callout"
        />
        <Input
          type="password"
          value={form.passphrase}
          onChange={(event) => setForm({ ...form, passphrase: event.target.value })}
          placeholder={t.syncAccountPassphrase}
          aria-label={t.syncAccountPassphrase}
          autoComplete="off"
          className="h-8 text-callout"
        />
        <Input
          value={form.device}
          onChange={(event) => setForm({ ...form, device: event.target.value })}
          placeholder={t.syncDeviceName}
          aria-label={t.syncDeviceName}
          autoComplete="off"
          className="h-8 text-callout"
        />
        <p className="text-callout text-label-3">{t.syncPassphraseHint}</p>
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
      <SettingsRow label={t.syncSignedInAs}>
        <span className="truncate text-callout text-label-2">{status.username}</span>
      </SettingsRow>
      <SettingsRow label={t.syncServerRow}>
        <span className="truncate font-mono text-callout text-label-2" title={status.server_url ?? ""}>
          {status.server_url}
        </span>
      </SettingsRow>
      <div className="flex items-start gap-2 px-3 py-2.5">
        <span className="shrink-0 text-callout text-label-1">{t.syncStatusRow}</span>
        <p role="status" className={cn("min-w-0 flex-1 text-right text-callout break-words", TONE[view.tone])}>
          {view.tone === "busy" && <LoaderCircle className="mr-1 inline size-3.5 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />}
          <span className="selectable">{statusText(view)}</span>
        </p>
      </div>
      <SettingsRow label={t.syncNow}>
        <Button
          variant="primary"
          size="sm"
          disabled={busy !== null || status.state === "syncing"}
          onClick={() => void run("now", syncNow)}
        >
          {busy === "now" ? <Spinner /> : <RefreshCw />}
          {t.syncNow}
        </Button>
      </SettingsRow>
      <SettingsRow label={t.syncSignOut}>
        <Button
          variant="secondary"
          size="sm"
          disabled={busy !== null}
          onClick={async () => {
            const ok = await confirm({ title: t.syncSignOutTitle, body: t.syncSignOutHint, verb: t.syncSignOut, destructive: false });
            if (ok) void run("signout", syncSignout);
          }}
        >
          <LogOut />
          {t.syncSignOut}
        </Button>
      </SettingsRow>
      <Message text={error ?? statusError} />
      {groups.length > 0 && (
        <div className="px-3 py-2.5">
          <p className="text-callout text-label-1">{t.syncSafetyNumber}</p>
          <p className="mt-0.5 text-callout text-label-2">{t.syncSafetyHint}</p>
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
      className="selectable mt-1.5 grid grid-cols-3 gap-x-3 gap-y-1 rounded-sm bg-fill-1 px-3 py-2 font-mono text-callout tabular-nums text-label-1"
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
  const confirm = useConfirm();

  const reload = useCallback(() => {
    if (!signedIn) return;
    syncDevices()
      .then(setDevices)
      .catch((reason) => onError(syncErrorText(reason)));
  }, [signedIn, onError]);
  useEffect(reload, [reload]);

  const revoke = async (id: string, name: string) => {
    const title = t.syncRevokeTitle.replace("{name}", name);
    if (!(await confirm({ title, body: t.syncRevokeHint, verb: t.syncRevoke }))) return;
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
              <p className={cn("truncate text-callout", device.revoked ? "text-label-3 line-through" : "text-label-1")}>
                {device.name}
              </p>
              <p className="text-callout text-label-3">
                {device.this_device
                  ? t.syncThisDevice
                  : device.revoked
                    ? device.revoked_at
                      ? `${t.syncRevoked} · ${new Date(device.revoked_at * 1000).toLocaleDateString()}`
                      : t.syncRevoked
                    : new Date(device.created_at * 1000).toLocaleDateString()}
              </p>
            </div>
            {!device.this_device && !device.revoked && (
              <Tooltip label={`${t.syncRevoke}: ${device.name}`}>
                <Button
                  variant="danger"
                  size="icon-sm"
                  aria-label={`${t.syncRevoke}: ${device.name}`}
                  onClick={() => void revoke(device.device_id, device.name)}
                >
                  <Ban />
                </Button>
              </Tooltip>
            )}
          </li>
        ))}
      </ul>
      <p className="mt-1 text-callout text-label-3">{t.syncRevokeHint}</p>
    </div>
  );
}

function Message({ text }: { text: string | null }) {
  if (!text) return null;
  return (
    <Notice className="m-3">{text}</Notice>
  );
}

function Spinner() {
  return <LoaderCircle className="animate-spin motion-reduce:animate-none" />;
}
