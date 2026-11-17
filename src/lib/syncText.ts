import { useCallback, useEffect, useState } from "react";
import { t } from "./i18n";
import { onSyncState, syncStatus, type SyncStatus } from "./syncApi";
import { ago, errorKey, type ErrorKey, type StatusView, type UrlCheck } from "./syncUi";

const ERROR_TEXT: Record<ErrorKey, string> = {
  wrongPassphrase: t.syncErrWrongPassphrase,
  unauthorized: t.syncErrUnauthorized,
  network: t.syncErrNetwork,
  unavailable: t.syncErrUnavailable,
  deviceRevoked: t.syncErrDeviceRevoked,
  registrationClosed: t.syncErrRegistrationClosed,
  fingerprintMismatch: t.syncErrFingerprintMismatch,
  vaultLocked: t.syncErrVaultLocked,
  forbidden: t.syncErrForbidden,
  rateLimited: t.syncErrRateLimited,
};

/** A message for an error from a sync command. Known codes get our wording;
 *  anything else shows what the daemon said. */
export function syncErrorText(reason: unknown): string {
  const code = reason && typeof reason === "object" && "code" in reason ? String((reason as { code: unknown }).code) : null;
  const key = errorKey(code);
  if (key) return ERROR_TEXT[key];
  if (reason instanceof Error) return reason.message;
  if (reason && typeof reason === "object" && "message" in reason) return String((reason as { message: unknown }).message);
  return String(reason);
}

export function urlErrorText(check: Extract<UrlCheck, { ok: false }>): string {
  return check.reason === "empty" ? t.syncUrlEmpty : check.reason === "insecure" ? t.syncUrlInsecure : t.syncUrlInvalid;
}

export function agoText(at: Date, now: Date = new Date()): string {
  const { unit, count } = ago(at, now);
  switch (unit) {
    case "now":
      return t.agoNow;
    case "minute":
      return t.agoMinutes.replace("{n}", String(count));
    case "hour":
      return t.agoHours.replace("{n}", String(count));
    default:
      return t.agoDays.replace("{n}", String(count));
  }
}

/** The status line: "Synced · 5 min ago", the error, and so on. */
export function statusText(view: StatusView): string {
  const when = view.lastSyncAt ? agoText(view.lastSyncAt) : null;
  const pending = view.pending > 0 ? `${view.pending} ${t.syncPending}` : null;
  const parts: (string | null)[] = (() => {
    switch (view.key) {
      case "signedOut":
        return [t.syncStatusSignedOut];
      case "neverSynced":
        return [t.syncStatusNever, pending];
      case "synced":
        return [t.syncStatusSynced, when, pending];
      case "syncing":
        return [t.syncStatusSyncing, pending];
      case "locked":
        return [t.syncStatusLocked, pending];
      default:
        return [t.syncStatusError, view.detail, when ? `${t.lastSync}: ${when}` : null];
    }
  })();
  return parts.filter(Boolean).join(" · ");
}

/** The sync status, kept current by the daemon's `sync.state` events. */
export function useSyncStatus(): { status: SyncStatus | null; error: string | null; refresh: () => void } {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const refresh = useCallback(() => {
    syncStatus()
      .then((next) => {
        setStatus(next);
        setError(null);
      })
      .catch((reason) => setError(syncErrorText(reason)));
  }, []);
  useEffect(() => {
    refresh();
    return onSyncState(setStatus);
  }, [refresh]);
  return { status, error, refresh };
}
