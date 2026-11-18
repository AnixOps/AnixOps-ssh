import { describe, expect, it, vi } from "vitest";

// The strings module reads the page's language when it loads; there is no page here.
vi.hoisted(() => {
  Object.assign(globalThis, { window: { location: { search: "?lang=en" } } });
});

import { t } from "./i18n";
import {
  BIOMETRIC,
  applySetup,
  backgroundLockChoices,
  backgroundLockLabel,
  biometricMethod,
  biometricReady,
  passphraseProblem,
  passphraseReady,
  vaultGate,
  watchLifecycle,
  type SetupApi,
  type VisibilitySource,
} from "./mobileVault";
import type { VaultStatus } from "./previewVault";

function status(over: Partial<VaultStatus> = {}): VaultStatus {
  return {
    passphraseSet: false,
    unlocked: true,
    autoLockMinutes: 15,
    backgroundLockSeconds: 0,
    setupDone: true,
    keychainAvailable: true,
    keychainError: null,
    unlockMethods: [],
    ...over,
  };
}

describe("vault gate", () => {
  it("waits for the first status", () => {
    expect(vaultGate(null)).toBe("loading");
  });

  it("asks the first-run question until it was answered", () => {
    expect(vaultGate(status({ setupDone: false }))).toBe("setup");
    expect(vaultGate(status({ setupDone: true }))).toBe("open");
  });

  it("never asks again once a passphrase exists", () => {
    expect(vaultGate(status({ setupDone: false, passphraseSet: true, unlocked: true }))).toBe("open");
  });

  it("locks over a passphrase vault that is closed", () => {
    expect(vaultGate(status({ passphraseSet: true, unlocked: false }))).toBe("locked");
  });

  it("leaves a device-only vault open", () => {
    expect(vaultGate(status({ passphraseSet: false, unlocked: false }))).toBe("open");
  });

  it("treats the desktop status, which has no setup flag, as answered", () => {
    expect(vaultGate(status({ setupDone: undefined }))).toBe("open");
  });
});

describe("biometrics", () => {
  const off = status({ unlockMethods: [{ name: BIOMETRIC, available: true, enabled: false }] });
  const on = status({ unlockMethods: [{ name: BIOMETRIC, available: true, enabled: true }] });
  const missing = status({ unlockMethods: [{ name: BIOMETRIC, available: false, enabled: false }] });

  it("is offered only where the phone has a sensor set up", () => {
    expect(biometricMethod(off)?.name).toBe(BIOMETRIC);
    expect(biometricMethod(missing)).toBeUndefined();
    expect(biometricMethod(status())).toBeUndefined();
    expect(biometricMethod(null)).toBeUndefined();
  });

  it("is ready only once turned on", () => {
    expect(biometricReady(on)).toBe(true);
    expect(biometricReady(off)).toBe(false);
    expect(biometricReady(missing)).toBe(false);
    expect(biometricReady(null)).toBe(false);
  });
});

describe("background lock choices", () => {
  it("offers the standard choices in order", () => {
    expect(backgroundLockChoices(0)).toEqual([0, 30, 60, 300, 900]);
  });

  it("keeps a saved value that is not a standard choice", () => {
    expect(backgroundLockChoices(120)).toEqual([0, 30, 60, 120, 300, 900]);
  });

  it("names every choice", () => {
    expect(backgroundLockLabel(0)).toBe(t.mvImmediately);
    expect(backgroundLockLabel(30)).toBe(t.mvAfter30s);
    expect(backgroundLockLabel(900)).toBe(t.mvAfter15m);
    expect(backgroundLockLabel(120)).toBe(t.mvAfterMinutes.replace("{n}", "2"));
    expect(backgroundLockLabel(45)).toBe(t.mvAfterSeconds.replace("{n}", "45"));
  });
});

describe("passphrase rules", () => {
  it("reports nothing before something was typed", () => {
    expect(passphraseProblem("", "")).toBeNull();
    expect(passphraseProblem("", "x")).toBeNull();
  });

  it("asks for eight characters", () => {
    expect(passphraseProblem("short", "")).toBe(t.vaultPassphraseShort);
    expect(passphraseProblem("long enough", "")).toBeNull();
  });

  it("counts characters, not bytes", () => {
    expect(passphraseReady("口令口令口令口令", "口令口令口令口令")).toBe(true);
    expect(passphraseReady("口令口令口令", "口令口令口令")).toBe(false);
  });

  it("notices a repetition that differs", () => {
    expect(passphraseProblem("long enough", "long enougx")).toBe(t.vaultPassphraseMismatch);
    expect(passphraseReady("long enough", "long enougx")).toBe(false);
    expect(passphraseReady("long enough", "")).toBe(false);
    expect(passphraseReady("long enough", "long enough")).toBe(true);
  });
});

describe("first-run setup", () => {
  const api = (over: Partial<SetupApi> = {}): SetupApi => ({
    setPassphrase: vi.fn(async () => status({ passphraseSet: true })),
    enableUnlock: vi.fn(async () =>
      status({ passphraseSet: true, unlockMethods: [{ name: BIOMETRIC, available: true, enabled: true }] }),
    ),
    skip: vi.fn(async () => status()),
    ...over,
  });

  it("skips without touching the passphrase", async () => {
    const calls = api();
    const result = await applySetup({ kind: "device" }, calls);
    expect(calls.skip).toHaveBeenCalledOnce();
    expect(calls.setPassphrase).not.toHaveBeenCalled();
    expect(result.biometricsFailed).toBeNull();
  });

  it("sets a passphrase alone", async () => {
    const calls = api();
    const result = await applySetup({ kind: "passphrase", passphrase: "long enough", biometrics: false }, calls);
    expect(calls.setPassphrase).toHaveBeenCalledWith("long enough");
    expect(calls.enableUnlock).not.toHaveBeenCalled();
    expect(result.status.passphraseSet).toBe(true);
  });

  it("adds biometrics on top of the passphrase", async () => {
    const calls = api();
    const result = await applySetup({ kind: "passphrase", passphrase: "long enough", biometrics: true }, calls);
    expect(calls.enableUnlock).toHaveBeenCalledWith(BIOMETRIC, "long enough");
    expect(biometricReady(result.status)).toBe(true);
    expect(result.biometricsFailed).toBeNull();
  });

  it("keeps the passphrase when the sensor prompt fails", async () => {
    const calls = api({
      enableUnlock: vi.fn(async () => {
        throw new Error("the unlock was cancelled");
      }),
    });
    const result = await applySetup({ kind: "passphrase", passphrase: "long enough", biometrics: true }, calls);
    expect(result.status.passphraseSet).toBe(true);
    expect(result.biometricsFailed).toContain("cancelled");
  });

  it("does not turn biometrics on when the passphrase fails", async () => {
    const calls = api({
      setPassphrase: vi.fn(async () => {
        throw new Error("no keychain");
      }),
    });
    await expect(applySetup({ kind: "passphrase", passphrase: "long enough", biometrics: true }, calls)).rejects.toThrow();
    expect(calls.enableUnlock).not.toHaveBeenCalled();
  });
});

describe("lifecycle watcher", () => {
  class FakeSource extends EventTarget implements VisibilitySource {
    visibilityState = "visible";
    set(state: string) {
      this.visibilityState = state;
      this.dispatchEvent(new Event("visibilitychange"));
    }
  }

  const setup = () => {
    const source = new FakeSource();
    const page = new EventTarget();
    const backend = { background: vi.fn(async () => {}), foreground: vi.fn(async () => {}) };
    const onForeground = vi.fn();
    const stop = watchLifecycle(source, backend, onForeground, page);
    return { source, page, backend, onForeground, stop };
  };
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

  it("reports leaving and returning once each", async () => {
    const { source, backend, onForeground } = setup();
    source.set("hidden");
    expect(backend.background).toHaveBeenCalledOnce();
    source.set("visible");
    await settle();
    expect(backend.foreground).toHaveBeenCalledOnce();
    expect(onForeground).toHaveBeenCalledOnce();
  });

  it("does not repeat a transition the platform signals twice", async () => {
    const { source, page, backend } = setup();
    source.set("hidden");
    page.dispatchEvent(new Event("pagehide"));
    expect(backend.background).toHaveBeenCalledOnce();
    page.dispatchEvent(new Event("pageshow"));
    source.set("visible");
    await settle();
    expect(backend.foreground).toHaveBeenCalledOnce();
  });

  it("covers a page that is put away without a visibility change", async () => {
    const { page, backend } = setup();
    page.dispatchEvent(new Event("pagehide"));
    expect(backend.background).toHaveBeenCalledOnce();
    page.dispatchEvent(new Event("pageshow"));
    await settle();
    expect(backend.foreground).toHaveBeenCalledOnce();
  });

  it("ignores a return that never left", () => {
    const { source, backend } = setup();
    source.set("visible");
    expect(backend.foreground).not.toHaveBeenCalled();
  });

  it("still refreshes when the backend fails to answer", async () => {
    const source = new FakeSource();
    const backend = {
      background: vi.fn(async () => {
        throw new Error("gone");
      }),
      foreground: vi.fn(async () => {
        throw new Error("gone");
      }),
    };
    const onForeground = vi.fn();
    watchLifecycle(source, backend, onForeground);
    source.set("hidden");
    source.set("visible");
    await settle();
    expect(onForeground).toHaveBeenCalledOnce();
  });

  it("stops watching", () => {
    const { source, backend, stop } = setup();
    stop();
    source.set("hidden");
    expect(backend.background).not.toHaveBeenCalled();
  });
});
