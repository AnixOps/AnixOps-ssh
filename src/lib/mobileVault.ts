import type { UnlockMethod, VaultStatus } from "./previewVault";
import { t } from "./i18n";

/** The unlock method the phone's fingerprint or face sensor provides. */
export const BIOMETRIC = "biometric";

/** Shortest vault passphrase the form accepts (the desktop's rule). */
export const MIN_PASSPHRASE = 8;

/** What the touch UI shows in front of everything else:
 *  `loading` until the first status arrives, `setup` on the first run, `locked`
 *  while a passphrase vault is closed, `open` otherwise. */
export type VaultGate = "loading" | "setup" | "locked" | "open";

export function vaultGate(status: VaultStatus | null): VaultGate {
  if (!status) return "loading";
  // A device-only vault opens itself and has nothing to ask; `setupDone` is
  // absent on the desktop, which has no first-run question.
  if (status.setupDone === false && !status.passphraseSet) return "setup";
  if (status.passphraseSet && !status.unlocked) return "locked";
  return "open";
}

/** The biometric method when this phone offers it right now. */
export function biometricMethod(
  status: VaultStatus | null,
): UnlockMethod | undefined {
  return status?.unlockMethods.find(
    (method) => method.name === BIOMETRIC && method.available,
  );
}

/** Biometrics are on and can unlock the vault now. */
export function biometricReady(status: VaultStatus | null): boolean {
  return biometricMethod(status)?.enabled === true;
}

/** Seconds in the background before the vault locks, as the picker offers
 *  them. 0 is "immediately", the default. */
export const BACKGROUND_LOCK_CHOICES = [0, 30, 60, 300, 900];

export function backgroundLockLabel(seconds: number): string {
  switch (seconds) {
    case 0:
      return t.mvImmediately;
    case 30:
      return t.mvAfter30s;
    case 60:
      return t.mvAfter1m;
    case 300:
      return t.mvAfter5m;
    case 900:
      return t.mvAfter15m;
    default:
      return seconds % 60 === 0
        ? t.mvAfterMinutes.replace("{n}", String(seconds / 60))
        : t.mvAfterSeconds.replace("{n}", String(seconds));
  }
}

/** The choices to show: the standard ones plus the saved value when it is not one of them. */
export function backgroundLockChoices(current: number): number[] {
  return BACKGROUND_LOCK_CHOICES.includes(current)
    ? BACKGROUND_LOCK_CHOICES
    : [...BACKGROUND_LOCK_CHOICES, current].sort((a, b) => a - b);
}

/** What is wrong with a new passphrase and its repetition, or null. Nothing is
 *  reported before something was typed. */
export function passphraseProblem(next: string, again: string): string | null {
  if (next === "") return null;
  if ([...next].length < MIN_PASSPHRASE) return t.vaultPassphraseShort;
  if (again !== "" && next !== again) return t.vaultPassphraseMismatch;
  return null;
}

export function passphraseReady(next: string, again: string): boolean {
  return [...next].length >= MIN_PASSPHRASE && next === again;
}

/** The two things the first-run form can do. */
export type SetupChoice =
  | { kind: "passphrase"; passphrase: string; biometrics: boolean }
  | { kind: "device" };

export interface SetupApi {
  setPassphrase(passphrase: string): Promise<VaultStatus>;
  enableUnlock(method: string, passphrase: string): Promise<VaultStatus>;
  skip(): Promise<VaultStatus>;
}

export interface SetupResult {
  status: VaultStatus;
  /** Biometrics were asked for and could not be turned on (cancelled or failed): the
   *  passphrase is set and works, and Settings can try again. */
  biometricsFailed: string | null;
}

/** Carries out a first-run choice. A passphrase comes first; biometrics are
 *  added on top, and failing to add them never undoes the passphrase. */
export async function applySetup(
  choice: SetupChoice,
  api: SetupApi,
): Promise<SetupResult> {
  if (choice.kind === "device")
    return { status: await api.skip(), biometricsFailed: null };
  let status = await api.setPassphrase(choice.passphrase);
  if (!choice.biometrics) return { status, biometricsFailed: null };
  try {
    status = await api.enableUnlock(BIOMETRIC, choice.passphrase);
    return { status, biometricsFailed: null };
  } catch (reason) {
    return { status, biometricsFailed: String(reason) };
  }
}

/** The pieces of a document the lifecycle watcher needs; a real `document` fits. */
export interface VisibilitySource extends EventTarget {
  visibilityState: string;
}

export interface LifecycleBackend {
  /** The app left the screen. */
  background(): Promise<unknown>;
  /** The app is back. */
  foreground(): Promise<unknown>;
}

/** Tells the backend when the app goes to the background and comes back, so the
 *  vault can lock (immediately by default, or after the chosen time). Each
 *  transition is reported once, however many events the platform fires for it.
 *  `onForeground` runs after the backend answered, to refresh what is shown.
 *  Returns the function that stops watching. */
export function watchLifecycle(
  source: VisibilitySource,
  backend: LifecycleBackend,
  onForeground: () => void = () => {},
  extra?: EventTarget,
): () => void {
  let away = false;
  const left = () => {
    if (away) return;
    away = true;
    void backend.background().catch(() => {});
  };
  const back = () => {
    if (!away) return;
    away = false;
    void backend
      .foreground()
      .catch(() => {})
      .finally(onForeground);
  };
  const onVisibility = () =>
    source.visibilityState === "hidden" ? left() : back();
  source.addEventListener("visibilitychange", onVisibility);
  // `pagehide` fires when iOS puts the page away without a visibility change.
  extra?.addEventListener("pagehide", left);
  extra?.addEventListener("pageshow", back);
  return () => {
    source.removeEventListener("visibilitychange", onVisibility);
    extra?.removeEventListener("pagehide", left);
    extra?.removeEventListener("pageshow", back);
  };
}
