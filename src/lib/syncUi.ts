import type { SyncStatus, VaultMember } from "./proto.gen";

/** Pure logic behind Settings → Sync and the team vaults page. No i18n and no
 *  backend calls here, so it stays testable; components map keys to strings. */

export type UrlCheck =
  | { ok: true; url: string }
  | { ok: false; reason: "empty" | "invalid" | "insecure" };

function isLoopback(host: string): boolean {
  const bare = host.replace(/^\[|\]$/g, "").toLowerCase();
  return bare === "localhost" || bare === "::1" || /^127(\.\d{1,3}){3}$/.test(bare);
}

/** The server must be https, except on this machine (a local test server).
 *  Mirrors the daemon's check, which is the one that counts. */
export function checkServerUrl(input: string): UrlCheck {
  const text = input.trim();
  if (!text) return { ok: false, reason: "empty" };
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return { ok: false, reason: "invalid" };
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return { ok: false, reason: "invalid" };
  if (!url.hostname) return { ok: false, reason: "invalid" };
  if (url.protocol === "http:" && !isLoopback(url.hostname)) return { ok: false, reason: "insecure" };
  return { ok: true, url: text.replace(/\/+$/, "") };
}

export type Tone = "neutral" | "ok" | "busy" | "warn" | "error";

export interface StatusView {
  key: "signedOut" | "synced" | "neverSynced" | "syncing" | "locked" | "error";
  tone: Tone;
  /** The time of the last successful sync, when there was one. */
  lastSyncAt: Date | null;
  /** The error text for `error`. */
  detail: string | null;
  /** Local changes waiting to be pushed. */
  pending: number;
}

/** What the status line shows. */
export function statusView(status: SyncStatus | null): StatusView {
  const parsed = status?.last_sync_at ? new Date(status.last_sync_at) : null;
  const lastSyncAt = parsed && !Number.isNaN(parsed.getTime()) ? parsed : null;
  const base = { lastSyncAt, detail: null, pending: status?.pending ?? 0 };
  if (!status || !status.signed_in) return { ...base, key: "signedOut", tone: "neutral", pending: 0 };
  switch (status.state) {
    case "syncing":
      return { ...base, key: "syncing", tone: "busy" };
    case "locked":
      return { ...base, key: "locked", tone: "warn" };
    case "error":
      return { ...base, key: "error", tone: "error", detail: status.last_error ?? null };
    default:
      return lastSyncAt
        ? { ...base, key: "synced", tone: "ok" }
        : { ...base, key: "neverSynced", tone: "neutral" };
  }
}

/** "Just now", "5 min ago" as a number and unit, for the caller to word. */
export function ago(at: Date, now: Date): { unit: "now" | "minute" | "hour" | "day"; count: number } {
  const seconds = Math.max(0, Math.floor((now.getTime() - at.getTime()) / 1000));
  if (seconds < 60) return { unit: "now", count: 0 };
  if (seconds < 3600) return { unit: "minute", count: Math.floor(seconds / 60) };
  if (seconds < 86400) return { unit: "hour", count: Math.floor(seconds / 3600) };
  return { unit: "day", count: Math.floor(seconds / 86400) };
}

/** Splits a safety number into its groups of digits for display. */
export function fingerprintGroups(fingerprint: string): string[] {
  return fingerprint.split(/\s+/).filter(Boolean);
}

/** Whether two safety numbers are the same, ignoring spaces and dashes. An
 *  empty one never matches. */
export function sameFingerprint(a: string, b: string): boolean {
  const x = a.replace(/\D/g, "");
  const y = b.replace(/\D/g, "");
  return x.length > 0 && x === y;
}

export type Role = VaultMember["role"];
export const ROLES: Role[] = ["owner", "editor", "viewer"];

/** Only owners change who is in a vault. */
export function canManageMembers(role: string | null | undefined): boolean {
  return role === "owner";
}

/** Whether the member's safety number still needs a look. */
export function needsAttention(trust: VaultMember["trust"]): boolean {
  return trust === "unconfirmed" || trust === "changed";
}

/** The last owner cannot be demoted or removed; the server refuses, this
 *  just keeps the controls honest. */
export function isLastOwner(members: VaultMember[], member: VaultMember): boolean {
  return member.role === "owner" && members.filter((m) => m.role === "owner").length === 1;
}

/** Display order: yourself, then by role, then by name. */
export function sortMembers(members: VaultMember[]): VaultMember[] {
  const rank = (m: VaultMember) => (m.trust === "self" ? -1 : ROLES.indexOf(m.role));
  return [...members].sort((a, b) => rank(a) - rank(b) || a.username.localeCompare(b.username));
}

/** A short text for an old value in the history list. Secret notes never show. */
export function historyValueText(field: string, value: unknown): string {
  if (field === "notes_secret") return "";
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  if (typeof value === "boolean" || typeof value === "number") return String(value);
  if (field === "target" && typeof value === "object") {
    const target = value as { kind?: string; alias?: string; hostname?: string; username?: string | null; port?: number };
    if (target.kind === "alias") return target.alias ?? "";
    const user = target.username ? `${target.username}@` : "";
    return `${user}${target.hostname ?? ""}${target.port && target.port !== 22 ? `:${target.port}` : ""}`;
  }
  if (Array.isArray(value)) return value.map((item) => (typeof item === "string" ? item : JSON.stringify(item))).join(", ");
  return JSON.stringify(value);
}

/** Daemon error codes the UI words itself; others show the daemon's text. */
export type ErrorKey =
  | "wrongPassphrase"
  | "unauthorized"
  | "network"
  | "unavailable"
  | "deviceRevoked"
  | "registrationClosed"
  | "fingerprintMismatch"
  | "vaultLocked"
  | "forbidden"
  | "rateLimited";

const ERROR_KEYS: Record<string, ErrorKey> = {
  wrong_passphrase: "wrongPassphrase",
  unauthorized: "unauthorized",
  network_error: "network",
  unavailable: "unavailable",
  device_revoked: "deviceRevoked",
  device_not_registered: "deviceRevoked",
  registration_closed: "registrationClosed",
  fingerprint_mismatch: "fingerprintMismatch",
  vault_locked: "vaultLocked",
  forbidden: "forbidden",
  rate_limited: "rateLimited",
};

export function errorKey(code: string | null | undefined): ErrorKey | null {
  return (code && ERROR_KEYS[code]) || null;
}
