import { useSyncExternalStore } from "react";
import { defaultLayout, sanitiseLayout, type BarLayout } from "./barLayout";
import { DEFAULT_GRACE_SECONDS, DEFAULT_KEEPALIVE_SECONDS, GRACE_CHOICES, KEEPALIVE_CHOICES } from "./backgroundPolicy";
import type { ImeMode } from "./imeMachine";
import { MAX_FONT, MIN_FONT } from "./touchGestures";

/* Preferences of the touch UI. They live in the web view's local storage: the
   phone's own settings, not synced, and the mobile app has no settings command
   in its backend. Every read and write is wrapped, because storage can be
   missing or full (private windows, previews), and the app has to work without. */

export type Attach = "off" | "tmux" | "screen";

export interface MobilePrefs {
  /** Terminal font size in px. */
  fontSize: number;
  haptics: boolean;
  imeMode: ImeMode;
  /** Show the second, swipeable row of keys. */
  extraRow: boolean;
  bar: BarLayout;
  /** Keep the connection this many seconds in the background; -1 for as long as the system allows. */
  graceSeconds: number;
  /** SSH keepalive interval in seconds; 0 turns it off. */
  keepaliveSeconds: number;
  /** Android only: show a notification while sessions are open so the system keeps the app alive. */
  foregroundService: boolean;
  /** Let a screen reader read the terminal (xterm's accessibility layer). */
  screenReader: boolean;
  /** Per host: attach to tmux or screen when the shell opens. */
  attach: Record<string, Attach>;
  /** Whether the first-use hint about the key bar has been dismissed. */
  hintDismissed: boolean;
}

export const DEFAULT_FONT_SIZE = 14;
const KEY = "anixops.mobile.prefs.v1";

export function defaultPrefs(): MobilePrefs {
  return {
    fontSize: DEFAULT_FONT_SIZE,
    haptics: true,
    imeMode: "auto",
    extraRow: true,
    bar: defaultLayout(),
    graceSeconds: DEFAULT_GRACE_SECONDS,
    keepaliveSeconds: DEFAULT_KEEPALIVE_SECONDS,
    foregroundService: false,
    screenReader: false,
    attach: {},
    hintDismissed: false,
  };
}

const pick = <T extends number>(value: unknown, choices: readonly T[], fallback: T): T =>
  typeof value === "number" && (choices as readonly number[]).includes(value) ? (value as T) : fallback;

/** Turns whatever was stored into valid preferences; anything wrong falls back to its default. */
export function sanitisePrefs(raw: unknown): MobilePrefs {
  const base = defaultPrefs();
  if (typeof raw !== "object" || raw === null) return base;
  const data = raw as Record<string, unknown>;
  const attach: Record<string, Attach> = {};
  if (typeof data.attach === "object" && data.attach !== null) {
    for (const [host, value] of Object.entries(data.attach)) {
      if (value === "tmux" || value === "screen") attach[host] = value;
    }
  }
  return {
    fontSize:
      typeof data.fontSize === "number" && Number.isFinite(data.fontSize)
        ? Math.min(MAX_FONT, Math.max(MIN_FONT, Math.round(data.fontSize)))
        : base.fontSize,
    haptics: typeof data.haptics === "boolean" ? data.haptics : base.haptics,
    imeMode: data.imeMode === "live" || data.imeMode === "hold" || data.imeMode === "auto" ? data.imeMode : base.imeMode,
    extraRow: typeof data.extraRow === "boolean" ? data.extraRow : base.extraRow,
    bar: data.bar === undefined ? base.bar : sanitiseLayout(data.bar),
    graceSeconds: pick(data.graceSeconds, GRACE_CHOICES, DEFAULT_GRACE_SECONDS),
    keepaliveSeconds: pick(data.keepaliveSeconds, KEEPALIVE_CHOICES, DEFAULT_KEEPALIVE_SECONDS),
    foregroundService: data.foregroundService === true,
    screenReader: data.screenReader === true,
    attach,
    hintDismissed: data.hintDismissed === true,
  };
}

function read(): MobilePrefs {
  try {
    const text = window.localStorage.getItem(KEY);
    return sanitisePrefs(text ? JSON.parse(text) : null);
  } catch {
    return defaultPrefs();
  }
}

let current: MobilePrefs | null = null;
const listeners = new Set<() => void>();

export function mobilePrefs(): MobilePrefs {
  current ??= read();
  return current;
}

export function updateMobilePrefs(patch: Partial<MobilePrefs>): MobilePrefs {
  current = sanitisePrefs({ ...mobilePrefs(), ...patch });
  try {
    window.localStorage.setItem(KEY, JSON.stringify(current));
  } catch {
    // Kept in memory for this run.
  }
  for (const listener of listeners) listener();
  return current;
}

export function setHostAttach(hostId: string, attach: Attach): MobilePrefs {
  const next = { ...mobilePrefs().attach };
  if (attach === "off") delete next[hostId];
  else next[hostId] = attach;
  return updateMobilePrefs({ attach: next });
}

export function hostAttach(hostId: string): Attach {
  return mobilePrefs().attach[hostId] ?? "off";
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useMobilePrefs(): MobilePrefs {
  return useSyncExternalStore(subscribe, mobilePrefs, mobilePrefs);
}

/** A short tick on a key press, where the system offers one (Android's web
 *  view does; iOS's does not and the call is a no-op there). */
export function haptic(prefs: MobilePrefs = mobilePrefs(), ms = 8) {
  if (!prefs.haptics) return;
  try {
    navigator.vibrate?.(ms);
  } catch {
    // Not available.
  }
}

/** The terminal font size scaled by the system's text-size setting where the
 *  web view reports it (iOS Dynamic Type through `-apple-system-body`). */
export function dynamicTypeScale(): number {
  try {
    if (typeof CSS === "undefined" || !CSS.supports("font: -apple-system-body")) return 1;
    const probe = document.createElement("span");
    probe.style.cssText = "font:-apple-system-body;position:absolute;visibility:hidden";
    probe.textContent = "A";
    document.body.appendChild(probe);
    const size = parseFloat(getComputedStyle(probe).fontSize);
    probe.remove();
    // 17 px is Dynamic Type's default body size.
    return Number.isFinite(size) && size > 0 ? Math.min(2, Math.max(0.8, size / 17)) : 1;
  } catch {
    return 1;
  }
}

/** True for a language whose input methods compose text (Chinese, Japanese, Korean). */
export function isCjkLanguage(language = typeof navigator === "undefined" ? "" : navigator.language): boolean {
  return /^(zh|ja|ko)\b/i.test(language);
}
