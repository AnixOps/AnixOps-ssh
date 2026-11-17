import { describe, expect, it } from "vitest";
import {
  ago,
  canManageMembers,
  checkServerUrl,
  errorKey,
  fingerprintGroups,
  historyValueText,
  isLastOwner,
  needsAttention,
  sameFingerprint,
  sortMembers,
  statusView,
} from "./syncUi";
import type { SyncStatus, VaultMember } from "./proto.gen";

const counts = { pulled: 0, pushed: 0, merged: 0, resolved: 0 };

function status(patch: Partial<SyncStatus>): SyncStatus {
  return {
    signed_in: true,
    state: "idle",
    server_url: "https://sync.example.com",
    username: "ana",
    device_name: "laptop",
    device_id: "d1",
    fingerprint: null,
    last_sync_at: null,
    last_error: null,
    last_error_code: null,
    pending: 0,
    last: counts,
    ...patch,
  };
}

function member(patch: Partial<VaultMember>): VaultMember {
  return { account_id: "a", username: "ana", role: "editor", fingerprint: "", trust: "confirmed", ...patch };
}

describe("checkServerUrl", () => {
  it("accepts https and trims the trailing slash", () => {
    expect(checkServerUrl(" https://sync.example.com/ ")).toEqual({ ok: true, url: "https://sync.example.com" });
  });
  it("accepts http only on this machine", () => {
    expect(checkServerUrl("http://127.0.0.1:8080").ok).toBe(true);
    expect(checkServerUrl("http://localhost:8080").ok).toBe(true);
    expect(checkServerUrl("http://[::1]:8080").ok).toBe(true);
    expect(checkServerUrl("http://sync.example.com")).toEqual({ ok: false, reason: "insecure" });
    expect(checkServerUrl("http://127.0.0.1.example.com")).toEqual({ ok: false, reason: "insecure" });
  });
  it("rejects empty and malformed input", () => {
    expect(checkServerUrl("  ")).toEqual({ ok: false, reason: "empty" });
    expect(checkServerUrl("sync.example.com")).toEqual({ ok: false, reason: "invalid" });
    expect(checkServerUrl("ftp://sync.example.com")).toEqual({ ok: false, reason: "invalid" });
  });
});

describe("statusView", () => {
  it("is signed out without an account", () => {
    expect(statusView(null).key).toBe("signedOut");
    expect(statusView(status({ signed_in: false })).key).toBe("signedOut");
  });
  it("tells synced from never synced", () => {
    expect(statusView(status({})).key).toBe("neverSynced");
    const view = statusView(status({ last_sync_at: "2026-01-02T03:04:05Z" }));
    expect(view.key).toBe("synced");
    expect(view.lastSyncAt?.toISOString()).toBe("2026-01-02T03:04:05.000Z");
  });
  it("carries the error and keeps the last success", () => {
    const view = statusView(status({ state: "error", last_error: "no route", last_sync_at: "2026-01-02T03:04:05Z" }));
    expect(view).toMatchObject({ key: "error", tone: "error", detail: "no route" });
    expect(view.lastSyncAt).not.toBeNull();
  });
  it("maps syncing and locked", () => {
    expect(statusView(status({ state: "syncing" })).tone).toBe("busy");
    expect(statusView(status({ state: "locked" })).tone).toBe("warn");
  });
  it("ignores an unreadable time", () => {
    expect(statusView(status({ last_sync_at: "yesterday" })).lastSyncAt).toBeNull();
  });
});

describe("ago", () => {
  const now = new Date("2026-01-02T12:00:00Z");
  it("picks the unit", () => {
    expect(ago(new Date("2026-01-02T11:59:30Z"), now)).toEqual({ unit: "now", count: 0 });
    expect(ago(new Date("2026-01-02T11:55:00Z"), now)).toEqual({ unit: "minute", count: 5 });
    expect(ago(new Date("2026-01-02T09:00:00Z"), now)).toEqual({ unit: "hour", count: 3 });
    expect(ago(new Date("2025-12-30T12:00:00Z"), now)).toEqual({ unit: "day", count: 3 });
  });
  it("never goes negative", () => {
    expect(ago(new Date("2026-01-02T12:00:10Z"), now).unit).toBe("now");
  });
});

describe("fingerprints", () => {
  const a = "12345 67890 11111 22222 33333 44444 55555 66666 77777 88888 99999 00000";
  it("splits into groups", () => {
    expect(fingerprintGroups(a)).toHaveLength(12);
  });
  it("compares digits only", () => {
    expect(sameFingerprint(a, a.replace(/ /g, "-"))).toBe(true);
    expect(sameFingerprint(a, a.replace("12345", "12346"))).toBe(false);
    expect(sameFingerprint("", "")).toBe(false);
  });
});

describe("members", () => {
  it("only owners manage", () => {
    expect(canManageMembers("owner")).toBe(true);
    expect(canManageMembers("editor")).toBe(false);
    expect(canManageMembers(null)).toBe(false);
  });
  it("flags unconfirmed and changed keys", () => {
    expect(needsAttention("unconfirmed")).toBe(true);
    expect(needsAttention("changed")).toBe(true);
    expect(needsAttention("confirmed")).toBe(false);
    expect(needsAttention("self")).toBe(false);
  });
  it("spots the last owner", () => {
    const owner = member({ role: "owner", username: "o" });
    expect(isLastOwner([owner, member({})], owner)).toBe(true);
    expect(isLastOwner([owner, member({ role: "owner" })], owner)).toBe(false);
  });
  it("sorts yourself first, then by role and name", () => {
    const list = [
      member({ username: "zed", role: "viewer" }),
      member({ username: "bob", role: "editor" }),
      member({ username: "me", role: "owner", trust: "self" }),
      member({ username: "amy", role: "editor" }),
    ];
    expect(sortMembers(list).map((m) => m.username)).toEqual(["me", "amy", "bob", "zed"]);
  });
});

describe("historyValueText", () => {
  it("shows plain values", () => {
    expect(historyValueText("name", "db")).toBe("db");
    expect(historyValueText("favorite", true)).toBe("true");
    expect(historyValueText("tags", ["a", "b"])).toBe("a, b");
    expect(historyValueText("color", null)).toBe("");
  });
  it("shows a target the way the list does", () => {
    expect(historyValueText("target", { kind: "endpoint", hostname: "h", username: "root", port: 2222 })).toBe(
      "root@h:2222",
    );
    expect(historyValueText("target", { kind: "alias", alias: "prod" })).toBe("prod");
  });
  it("never shows secret notes", () => {
    expect(historyValueText("notes_secret", "hunter2")).toBe("");
  });
});

describe("errorKey", () => {
  it("maps known codes only", () => {
    expect(errorKey("wrong_passphrase")).toBe("wrongPassphrase");
    expect(errorKey("network_error")).toBe("network");
    expect(errorKey("something_else")).toBeNull();
    expect(errorKey(null)).toBeNull();
  });
});
