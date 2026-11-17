import type {
  HostHistoryEntry,
  SyncDevice,
  SyncStatus,
  VaultInfo,
  VaultMember,
} from "./proto.gen";

/** In-memory stand-in for the daemon's sync commands in browser previews and
 *  CI screenshots. `?sync=in` starts signed in with a team vault. */
const FINGERPRINT = "48213 90577 31264 85509 12746 66380 05931 77842 20915 43368 81207 59604";

const signedIn = new URLSearchParams(window.location.search).get("sync") === "in";
const empty = { pulled: 0, pushed: 0, merged: 0, resolved: 0 };

let status: SyncStatus = signedIn ? inStatus() : outStatus();
let devices: SyncDevice[] = [];
let vaults: VaultInfo[] = [{ id: "personal", name: "Personal", kind: "personal", role: "owner" }];
const members = new Map<string, VaultMember[]>();
const listeners = new Set<(status: SyncStatus) => void>();
const history: HostHistoryEntry[] = [
  { seq: 2, field: "name", value: "web-1", changed_at: "2026-11-02T09:15:00Z", device: "Work laptop" },
  { seq: 1, field: "tags", value: ["prod"], changed_at: "2026-10-28T18:40:00Z", device: "Home desktop" },
];

function outStatus(): SyncStatus {
  return {
    signed_in: false,
    state: "signed_out",
    server_url: null,
    username: null,
    device_name: null,
    device_id: null,
    fingerprint: null,
    last_sync_at: null,
    last_error: null,
    last_error_code: null,
    pending: 0,
    last: empty,
  };
}

function inStatus(): SyncStatus {
  return {
    signed_in: true,
    state: "idle",
    server_url: "https://sync.example.com",
    username: "you",
    device_name: "This device",
    device_id: "dev-1",
    fingerprint: FINGERPRINT,
    last_sync_at: new Date().toISOString(),
    last_error: null,
    last_error_code: null,
    pending: 0,
    last: empty,
  };
}

function seed() {
  devices = [
    { device_id: "dev-1", name: "This device", created_at: 1_760_000_000, revoked: false, this_device: true },
    { device_id: "dev-2", name: "Work laptop", created_at: 1_761_000_000, revoked: false, this_device: false },
  ];
  vaults = [
    { id: "personal", name: "Personal", kind: "personal", role: "owner" },
    { id: "team-1", name: "Platform team", kind: "team", role: "owner" },
  ];
  members.set("team-1", [
    { account_id: "acc-me", username: "you", role: "owner", fingerprint: FINGERPRINT, trust: "self" },
    {
      account_id: "acc-2",
      username: "mira",
      role: "editor",
      fingerprint: "11111 22222 33333 44444 55555 66666 77777 88888 99999 00000 12121 34343",
      trust: "unconfirmed",
    },
  ]);
}
if (signedIn) seed();

function fail(code: string, message: string): never {
  throw Object.assign(new Error(message), { code });
}

function publish(next: SyncStatus): SyncStatus {
  status = next;
  for (const listener of listeners) listener(status);
  return status;
}

export const previewSync = {
  onState(handler: (status: SyncStatus) => void): () => void {
    listeners.add(handler);
    return () => void listeners.delete(handler);
  },

  async call(kind: string, payload: object): Promise<unknown> {
    const p = payload as Record<string, string>;
    switch (kind) {
      case "sync.status":
        return status;
      case "sync.signup":
      case "sync.signin": {
        if (p.passphrase === "wrong") fail("wrong_passphrase", "wrong account passphrase");
        seed();
        return publish({ ...inStatus(), server_url: p.server_url, username: p.username });
      }
      case "sync.signout":
        devices = [];
        publish(outStatus());
        return null;
      case "sync.now":
        return publish({ ...status, last_sync_at: new Date().toISOString(), state: "idle" });
      case "sync.devices.list":
        return { devices };
      case "sync.devices.revoke":
        devices = devices.map((d) => (d.device_id === p.device_id ? { ...d, revoked: true } : d));
        return null;
      case "vaults.list":
        return { vaults };
      case "vault.create_shared": {
        const vault: VaultInfo = { id: `team-${vaults.length + 1}`, name: p.name, kind: "team", role: "owner" };
        vaults = [...vaults, vault];
        members.set(vault.id, [
          { account_id: "acc-me", username: "you", role: "owner", fingerprint: FINGERPRINT, trust: "self" },
        ]);
        return vault;
      }
      case "vault.members.list":
        return { members: members.get(p.vault_id) ?? [] };
      case "vault.members.lookup":
        if (p.username === "nobody") fail("not_found", "no such account");
        return { account_id: `acc-${p.username}`, username: p.username, fingerprint: "24680 13579 24680 13579 24680 13579 24680 13579 24680 13579 24680 13579" };
      case "vault.members.add": {
        const member: VaultMember = {
          account_id: `acc-${p.username}`,
          username: p.username,
          role: p.role as VaultMember["role"],
          fingerprint: p.fingerprint,
          trust: "confirmed",
        };
        members.set(p.vault_id, [...(members.get(p.vault_id) ?? []), member]);
        return member;
      }
      case "vault.members.remove":
        members.set(p.vault_id, (members.get(p.vault_id) ?? []).filter((m) => m.account_id !== p.account_id));
        return null;
      case "vault.members.role":
        members.set(
          p.vault_id,
          (members.get(p.vault_id) ?? []).map((m) =>
            m.account_id === p.account_id ? { ...m, role: p.role as VaultMember["role"] } : m,
          ),
        );
        return null;
      case "hosts.vaults":
        return { host_vaults: {} };
      case "hosts.move_vault":
        return { id: p.id };
      case "hosts.history":
        return { entries: signedIn || status.signed_in ? history : [] };
      case "hosts.history_restore":
        return null;
      default:
        return fail("invalid_request", `unknown command ${kind}`);
    }
  },
};
