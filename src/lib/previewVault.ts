import type { SecretsInput, SecretsStatus } from "./backend";

/** Windows Hello, Touch ID: mirrors `UnlockMethodView` in src-tauri/src/vault.rs. */
export interface UnlockMethod {
  /** `windows_hello`, `touch_id`, or `biometric` (the phone's fingerprint or face). */
  name: string;
  /** The OS offers it on this device right now. */
  available: boolean;
  /** The user turned it on. */
  enabled: boolean;
  /** The removable entries behind it: the security keys added to the vault. */
  items?: UnlockItem[];
}

/** One security key added to the vault; mirrors `UnlockItemView`. */
export interface UnlockItem {
  id: string;
  label: string;
}

/** Mirrors `VaultStatusView` in src-tauri/src/vault.rs. */
export interface VaultStatus {
  passphraseSet: boolean;
  unlocked: boolean;
  /** 0 = never. */
  autoLockMinutes: number;
  keychainAvailable: boolean;
  keychainError: string | null;
  unlockMethods: UnlockMethod[];
  /** Mobile only: seconds in the background before the vault locks (0 = at once). */
  backgroundLockSeconds?: number;
  /** Mobile only: false until the first-run question was answered. */
  setupDone?: boolean;
}

/** In-memory stand-in for the vault in browser previews and CI screenshots.
 *  The touch UI adds `?vault=first-run` (nothing chosen yet), `?vault=bio`
 *  (a passphrase and biometrics on, the vault locked) and `?vault=bio-off`
 *  (a passphrase, biometrics available but off).
 *  Values are kept only to track which secrets exist, as the real backend
 *  never returns them either. `?vault=locked` starts with a passphrase set
 *  and the vault locked; `?vault=nokeychain` simulates a machine without one;
 *  `?vault=hello` has a passphrase and Windows Hello available but off;
 *  `?vault=hello-on` has it on and the vault locked. `?vault=key` has a
 *  passphrase and security keys available with none added, `?vault=key-on`
 *  has two added and the vault locked, `?vault=key-none` has a system that
 *  cannot use security keys. */
const mode = new URLSearchParams(window.location.search).get("vault");
const hello = mode === "hello" || mode === "hello-on";
const bio = mode === "bio" || mode === "bio-off";
const key = mode === "key" || mode === "key-on" || mode === "key-none";
let state: VaultStatus = {
  passphraseSet: mode === "locked" || hello || bio || key,
  unlocked: mode !== "locked" && mode !== "hello-on" && mode !== "bio" && mode !== "key-on",
  autoLockMinutes: 15,
  backgroundLockSeconds: 0,
  setupDone: mode !== "first-run",
  keychainAvailable: mode !== "nokeychain",
  keychainError: mode === "nokeychain" ? "no secret service is running" : null,
  unlockMethods: hello
    ? [{ name: "windows_hello", available: true, enabled: mode === "hello-on" }]
    : key
      ? [
          {
            name: "security_key",
            available: mode !== "key-none",
            enabled: mode === "key-on",
            items:
              mode === "key-on"
                ? [
                    { id: "k1", label: "YubiKey 5C" },
                    { id: "k2", label: "Backup key" },
                  ]
                : [],
          },
        ]
      : bio
      ? [{ name: "biometric", available: true, enabled: mode === "bio" }]
      : mode === "first-run"
        ? [{ name: "biometric", available: true, enabled: false }]
        : [],
};
let passphrase = mode === "locked" || hello || bio || key ? "preview-passphrase" : "";
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
    state = { ...state, passphraseSet: true, unlocked: true, setupDone: true };
    return changed();
  },
  removePassphrase: async (given: string): Promise<VaultStatus> => {
    if (given !== passphrase) wrong();
    passphrase = "";
    state = {
      ...state,
      passphraseSet: false,
      unlocked: true,
      unlockMethods: state.unlockMethods.map((m) => ({ ...m, enabled: false, items: m.items ? [] : m.items })),
    };
    return changed();
  },
  unlockWith: async (method: string): Promise<VaultStatus> => {
    // `?vault=bio` stands for a sensor prompt that was dismissed, so the lock
    // screen stays up for its screenshot; the passphrase still opens it.
    if (mode === "bio" || !state.unlockMethods.some((m) => m.name === method && m.enabled)) {
      throw new Error("the unlock was cancelled");
    }
    state = { ...state, unlocked: true };
    return changed();
  },
  enableUnlock: async (method: string, given: string): Promise<VaultStatus> => {
    if (given !== passphrase) wrong();
    state = { ...state, unlockMethods: state.unlockMethods.map((m) => (m.name === method ? { ...m, enabled: true } : m)) };
    return changed();
  },
  disableUnlock: async (method: string): Promise<VaultStatus> => {
    state = {
      ...state,
      unlockMethods: state.unlockMethods.map((m) =>
        m.name === method ? { ...m, enabled: false, items: m.items ? [] : m.items } : m,
      ),
    };
    return changed();
  },
  addUnlockItem: async (method: string, given: string, label: string): Promise<VaultStatus> => {
    if (given !== passphrase) wrong();
    state = {
      ...state,
      unlockMethods: state.unlockMethods.map((m) => {
        if (m.name !== method) return m;
        const items = [...(m.items ?? [])];
        if (items.length >= 4) throw new Error("at most 4 security keys can be added; remove one first");
        items.push({ id: `k${Date.now()}`, label: label.trim() || `Security key ${items.length + 1}` });
        return { ...m, enabled: true, items };
      }),
    };
    return changed();
  },
  removeUnlockItem: async (method: string, id: string): Promise<VaultStatus> => {
    state = {
      ...state,
      unlockMethods: state.unlockMethods.map((m) => {
        if (m.name !== method) return m;
        const items = (m.items ?? []).filter((item) => item.id !== id);
        return { ...m, enabled: items.length > 0, items };
      }),
    };
    return changed();
  },
  setAutoLock: async (minutes: number): Promise<VaultStatus> => {
    state = { ...state, autoLockMinutes: minutes };
    return { ...state };
  },
  setBackgroundLock: async (seconds: number): Promise<VaultStatus> => {
    state = { ...state, backgroundLockSeconds: seconds };
    return { ...state };
  },
  setupSkip: async (): Promise<VaultStatus> => {
    state = { ...state, setupDone: true };
    return changed();
  },
  /** The app left the screen; a passphrase vault locks at once in the preview. */
  background: async (): Promise<void> => {
    if (
      state.passphraseSet &&
      (state.backgroundLockSeconds ?? 0) === 0 &&
      state.unlocked
    ) {
      state = { ...state, unlocked: false };
      changed();
    }
  },
  foreground: async (): Promise<void> => {},
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
