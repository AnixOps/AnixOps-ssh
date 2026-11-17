import { t } from "./i18n";

const listeners = new Set<() => void>();

/** Asks the app to show the "unlock the vault" dialog (a saved secret is needed). */
export function requestVaultUnlock(): void {
  for (const listener of listeners) listener();
}

export function onVaultUnlockRequested(handler: () => void): () => void {
  listeners.add(handler);
  return () => void listeners.delete(handler);
}

/** A readable message for a failed vault action; never includes anything typed. */
export function vaultErrorText(reason: unknown): string {
  const text = String(reason);
  if (text.includes("wrong vault passphrase")) return t.vaultWrongPassphrase;
  if (text.includes("the unlock was cancelled")) return t.unlockCancelled;
  if (text.includes("the system unlock failed")) return `${t.unlockFailed}: ${text.split(": ").slice(1).join(": ")}`;
  return text;
}

/** The name of an unlock method (Windows Hello, Touch ID). */
export function unlockMethodLabel(name: string): string {
  return name === "windows_hello" ? t.unlockWindowsHello : name === "touch_id" ? t.unlockTouchId : name;
}

/** The unlock button's text for a method. */
export function unlockWithLabel(name: string): string {
  return name === "windows_hello" ? t.unlockWithWindowsHello : name === "touch_id" ? t.unlockWithTouchId : name;
}

