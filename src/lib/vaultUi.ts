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
  return text.includes("wrong vault passphrase") ? t.vaultWrongPassphrase : text;
}
