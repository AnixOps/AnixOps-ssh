import { beforeAll, describe, expect, it, vi } from "vitest";

// The modules read the language and the preview mode from the page URL; the
// tests run in node, so give them one (English, no preview mode).
vi.stubGlobal("window", { location: { search: "?lang=en" } });

const { t } = await import("./i18n");
const { previewVault } = await import("./previewVault");
const { securityKeyErrorText, unlockMethodLabel, unlockWithLabel, vaultErrorText } = await import("./vaultUi");

beforeAll(() => {
  expect(t.vaultUnlock).toBeTruthy();
});

describe("security key vault unlock texts", () => {
  it("names the method and its unlock button", () => {
    expect(unlockMethodLabel("security_key")).toBe(t.unlockSecurityKey);
    expect(unlockWithLabel("security_key")).toBe(t.unlockWithSecurityKey);
    // The older methods are unchanged.
    expect(unlockMethodLabel("windows_hello")).toBe(t.unlockWindowsHello);
  });

  it("turns the daemon's English details into something the user can act on", () => {
    const wrap = (detail: string) => `the system unlock failed: ${detail}`;
    expect(vaultErrorText(wrap("no security key is connected"))).toBe(t.securityKeyVaultNoDevice);
    expect(
      vaultErrorText(wrap("this security key does not support hmac-secret, or this system cannot use it")),
    ).toBe(t.securityKeyVaultNoHmac);
    expect(vaultErrorText(wrap("the security key needs its PIN"))).toBe(t.securityKeyVaultNeedsPin);
    expect(vaultErrorText(wrap("the security key PIN is blocked"))).toBe(t.securityKeyVaultPinBlocked);
    expect(vaultErrorText(wrap("wrong security key PIN (2 tries left)"))).toBe(t.securityKeyVaultWrongPin);
    expect(vaultErrorText(wrap("the connected security key is not one added to the vault"))).toBe(
      t.securityKeyVaultWrongKey,
    );
    expect(vaultErrorText("at most 4 security keys can be added; remove one first")).toBe(t.securityKeyVaultLimit);
    // Anything else keeps the generic text with the detail.
    expect(vaultErrorText(wrap("something else"))).toBe(`${t.unlockFailed}: something else`);
    expect(securityKeyErrorText("something else")).toBeNull();
  });

  it("keeps the passphrase errors as they were", () => {
    expect(vaultErrorText("wrong vault passphrase")).toBe(t.vaultWrongPassphrase);
    expect(vaultErrorText("the unlock was cancelled")).toBe(t.unlockCancelled);
  });
});

describe("the preview vault's security keys", () => {
  it("needs the vault passphrase to add a key and ignores removing one that is not there", async () => {
    const before = await previewVault.status();
    await expect(previewVault.addUnlockItem("security_key", "wrong", "k")).rejects.toThrow("wrong vault passphrase");
    const after = await previewVault.removeUnlockItem("security_key", "x");
    expect(after.unlockMethods).toEqual(before.unlockMethods);
  });
});
