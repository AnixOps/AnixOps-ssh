import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isTauri } from "./backend";
import type {
  HostHistoryEntry,
  SyncDevice,
  SyncStatus,
  VaultInfo,
  VaultMember,
  VaultMembersLookupResult,
} from "./proto.gen";
import { previewSync } from "./previewSync";

export type { HostHistoryEntry, SyncDevice, SyncStatus, VaultInfo, VaultMember, VaultMembersLookupResult };

/** An error from the daemon: a stable `code` plus the message. */
export class SyncCallError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

function asError(reason: unknown): SyncCallError {
  if (reason instanceof SyncCallError) return reason;
  if (reason && typeof reason === "object" && "code" in reason && "message" in reason) {
    const { code, message } = reason as { code: unknown; message: unknown };
    return new SyncCallError(String(code), String(message));
  }
  return new SyncCallError("unknown", String(reason));
}

/** Runs one of the daemon's sync commands. Sync lives in the background
 *  service, so outside the app (the browser preview) a stand-in answers. */
async function call<T>(kind: string, payload: object = {}): Promise<T> {
  try {
    if (!isTauri) return (await previewSync.call(kind, payload)) as T;
    return await invoke<T>("sync_call", { kind, payload });
  } catch (reason) {
    throw asError(reason);
  }
}

export const syncStatus = () => call<SyncStatus>("sync.status");

export const syncSignup = (serverUrl: string, username: string, passphrase: string, deviceName?: string) =>
  call<SyncStatus>("sync.signup", { server_url: serverUrl, username, passphrase, device_name: deviceName });

export const syncSignin = (serverUrl: string, username: string, passphrase: string, deviceName?: string) =>
  call<SyncStatus>("sync.signin", { server_url: serverUrl, username, passphrase, device_name: deviceName });

export const syncSignout = () => call<null>("sync.signout");

/** Runs a sync now and returns the status afterwards. */
export const syncNow = () => call<SyncStatus>("sync.now");

export const syncDevices = async () => (await call<{ devices: SyncDevice[] }>("sync.devices.list")).devices;

export const syncRevokeDevice = (deviceId: string) => call<null>("sync.devices.revoke", { device_id: deviceId });

export const listVaults = async () => (await call<{ vaults: VaultInfo[] }>("vaults.list")).vaults;

export const createSharedVault = (name: string) => call<VaultInfo>("vault.create_shared", { name });

export const vaultMembers = async (vaultId: string) =>
  (await call<{ members: VaultMember[] }>("vault.members.list", { vault_id: vaultId })).members;

/** The keys the server holds for an account, as a safety number to compare. */
export const lookupMember = (username: string) => call<VaultMembersLookupResult>("vault.members.lookup", { username });

export const addMember = (vaultId: string, username: string, role: VaultMember["role"], fingerprint: string) =>
  call<VaultMember>("vault.members.add", { vault_id: vaultId, username, role, fingerprint });

export const removeMember = (vaultId: string, accountId: string) =>
  call<null>("vault.members.remove", { vault_id: vaultId, account_id: accountId });

export const setMemberRole = (vaultId: string, accountId: string, role: VaultMember["role"]) =>
  call<null>("vault.members.role", { vault_id: vaultId, account_id: accountId, role });

/** The vault of each host that is not in the personal one (host id to vault id). */
export const hostVaults = async () => (await call<{ host_vaults: Record<string, string> }>("hosts.vaults")).host_vaults;

/** Moves a host into a vault; it gets a new id, which is returned. */
export const moveHostToVault = async (id: string, vaultId: string) =>
  (await call<{ id: string }>("hosts.move_vault", { id, vault_id: vaultId })).id;

export const hostHistory = async (id: string) => (await call<{ entries: HostHistoryEntry[] }>("hosts.history", { id })).entries;

export const restoreHostHistory = (id: string, seq: number) => call<null>("hosts.history_restore", { id, seq });

/** Subscribes to sync state changes; returns the unsubscribe function. */
export function onSyncState(handler: (status: SyncStatus) => void): () => void {
  if (!isTauri) return previewSync.onState(handler);
  const stop = listen<SyncStatus>("sync-state", (event) => handler(event.payload));
  return () => void stop.then((unlisten) => unlisten());
}
