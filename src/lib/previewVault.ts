import type { SecretsInput, SecretsStatus } from "./backend";

/** Mirrors `VaultStatusView` in src-tauri/src/vault.rs. */
export interface VaultStatus {
  passphraseSet: boolean;
  unlocked: boolean;
  /** 0 = never. */
  autoLockMinutes: number;
  keychainAvailable: boolean;
  keychainError: string | null;
}

/** In-memory stand-in for the vault in browser previews and CI screenshots.
 *  Values are kept only to track which secrets exist, as the real backend
 *  never returns them either. `?vault=locked` starts with a passphrase set
 *  and the vault locked; `?vault=nokeychain` simulates a machine without one. */
const mode = new URLSearchParams(window.location.search).get("vault");
let state: VaultStatus = {
  passphraseSet: mode === "locked",
  unlocked: mode !== "locked",
  autoLockMinutes: 15,
  keychainAvailable: mode !== "nokeychain",
  keychainError: mode === "nokeychain" ? "no secret service is running" : null,
};
let passphrase = mode === "locked" ? "preview-passphrase" : "";
const saved = new Map<string, SecretsStatus>([["3", { password: true, privateKey: false, passphrase: false }]]);
const listeners = new Set<(unlocked: boolean) => void>();

function changed(): VaultStatus {
  for (const listener of listeners) listener(state.unlocked);
  return { ...state };
}

function wrong(): never {
  throw new Error("wrong vault passphrase");
}

export const previewVault = {
  status: async (): Promise<VaultStatus> => ({ ...state }),
  unlock: async (given: string): Promise<VaultStatus> => {
    if (state.passphraseSet && given !== passphrase) wrong();
    state = { ...state, unlocked: true };
    return changed();
  },
  lock: async (): Promise<VaultStatus> => {
    state = { ...state, unlocked: false };
    return changed();
  },
  setPassphrase: async (next: string, current: string | null): Promise<VaultStatus> => {
    if (state.passphraseSet && current !== passphrase) wrong();
    passphrase = next;
    state = { ...state, passphraseSet: true, unlocked: true };
    return changed();
  },
  removePassphrase: async (given: string): Promise<VaultStatus> => {
    if (given !== passphrase) wrong();
    passphrase = "";
    state = { ...state, passphraseSet: false, unlocked: true };
    return changed();
  },
  setAutoLock: async (minutes: number): Promise<VaultStatus> => {
    state = { ...state, autoLockMinutes: minutes };
    return { ...state };
  },
  onChanged: (handler: (unlocked: boolean) => void): (() => void) => {
    listeners.add(handler);
    return () => void listeners.delete(handler);
  },
};

export async function previewForgetAll(): Promise<number> {
  const hosts = [...saved.values()].filter((s) => s.password || s.privateKey || s.passphrase).length;
  saved.clear();
  return hosts;
}

export async function previewSecretsStatus(hostId: string): Promise<SecretsStatus> {
  return { ...(saved.get(hostId) ?? { password: false, privateKey: false, passphrase: false }) };
}

export async function previewSecretsSet(hostId: string, input: SecretsInput): Promise<SecretsStatus> {
  if (!state.keychainAvailable) throw new Error(`the operating system keychain is not available: ${state.keychainError}`);
  if (state.passphraseSet && !state.unlocked) throw new Error("the vault is locked");
  const current = saved.get(hostId) ?? { password: false, privateKey: false, passphrase: false };
  const next = {
    password: input.password === undefined ? current.password : input.password !== "",
    privateKey: input.privateKey === undefined ? current.privateKey : input.privateKey !== "",
    passphrase: input.passphrase === undefined ? current.passphrase : input.passphrase !== "",
  };
  saved.set(hostId, next);
  return { ...next };
}
