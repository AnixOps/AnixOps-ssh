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
  if (text.includes("the system unlock failed")) {
    const detail = text.split(": ").slice(1).join(": ");
    return securityKeyErrorText(detail) ?? `${t.unlockFailed}: ${detail}`;
  }
  if (text.includes("security keys can be added")) return t.securityKeyVaultLimit;
  return text;
}

/** What a failed security key step means for the user, from the daemon's
 *  English detail; `null` for anything else. */
export function securityKeyErrorText(detail: string): string | null {
  const known: [string, string][] = [
    ["no security key is connected", t.securityKeyVaultNoDevice],
    ["does not support hmac-secret", t.securityKeyVaultNoHmac],
    ["needs its PIN", t.securityKeyVaultNeedsPin],
    ["PIN is blocked", t.securityKeyVaultPinBlocked],
    ["wrong security key PIN", t.securityKeyVaultWrongPin],
    ["not one added to the vault", t.securityKeyVaultWrongKey],
    ["answered differently", t.securityKeyVaultChanged],
  ];
  return known.find(([needle]) => detail.includes(needle))?.[1] ?? null;
}

/** The name of an unlock method (Windows Hello, Touch ID, security keys). */
export function unlockMethodLabel(name: string): string {
  if (name === "security_key") return t.unlockSecurityKey;
  if (name === "biometric") return t.unlockBiometric;
  return name === "windows_hello" ? t.unlockWindowsHello : name === "touch_id" ? t.unlockTouchId : name;
}

/** The unlock button's text for a method. */
export function unlockWithLabel(name: string): string {
  if (name === "security_key") return t.unlockWithSecurityKey;
  if (name === "biometric") return t.unlockWithBiometric;
  return name === "windows_hello" ? t.unlockWithWindowsHello : name === "touch_id" ? t.unlockWithTouchId : name;
}

