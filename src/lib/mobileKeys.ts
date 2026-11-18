/* What the on-screen key bar sends. A soft keyboard has no Esc, Tab, Ctrl,
   arrows or function keys; xterm.js cannot produce them from touch, so the bar
   writes the same bytes a hardware keyboard would (xterm's own encoding). */

export type ArrowKey = "up" | "down" | "left" | "right";
export type FunctionKey = "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12";
export type BarKey =
  | "esc"
  | "tab"
  | ArrowKey
  | "home"
  | "end"
  | "pgup"
  | "pgdn"
  | "ins"
  | "del"
  | FunctionKey;

export const FUNCTION_KEYS: FunctionKey[] = ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12"];

/** Modifier keys held for one key press (or locked). Shift is not on the bar:
 *  the soft keyboard has it. */
export interface Modifiers {
  ctrl: boolean;
  alt: boolean;
}

const NO_MODIFIERS: Modifiers = { ctrl: false, alt: false };

const ARROWS: Record<ArrowKey, string> = { up: "A", down: "B", right: "C", left: "D" };
const HOME_END: Record<"home" | "end", string> = { home: "H", end: "F" };
/** `ESC [ n ~` keys. */
const TILDE: Record<"ins" | "del" | "pgup" | "pgdn", number> = { ins: 2, del: 3, pgup: 5, pgdn: 6 };
const F_SS3: Record<"f1" | "f2" | "f3" | "f4", string> = { f1: "P", f2: "Q", f3: "R", f4: "S" };
const F_TILDE: Record<"f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12", number> = {
  f5: 15,
  f6: 17,
  f7: 18,
  f8: 19,
  f9: 20,
  f10: 21,
  f11: 23,
  f12: 24,
};

/** xterm's modifier parameter: 1 + shift 1 + alt 2 + ctrl 4 (`ESC [ 1 ; 5 A` is Ctrl+Up). */
export function modifierParam({ ctrl, alt }: Modifiers): number {
  return 1 + (alt ? 2 : 0) + (ctrl ? 4 : 0);
}

/** The bytes for a bar key. Arrows, Home and End follow the terminal's
 *  cursor-key mode (full-screen programs such as vim and less switch it on)
 *  and, with a modifier armed, use the xterm form with the modifier
 *  parameter, which is what shells and editors expect. Alt on Esc and Tab is
 *  the usual ESC prefix. `ctrl` and `alt` may be passed as booleans (the old
 *  signature) or as one `Modifiers`. */
export function keySequence(
  key: BarKey,
  applicationCursor: boolean,
  ctrl: boolean | Modifiers,
  alt = false,
): string {
  const mods: Modifiers = typeof ctrl === "object" ? ctrl : { ctrl, alt };
  const modified = mods.ctrl || mods.alt;
  const param = modifierParam(mods);

  if (key === "esc") return mods.alt ? "\x1b\x1b" : "\x1b";
  if (key === "tab") return mods.alt ? "\x1b\t" : "\t";
  if (key === "up" || key === "down" || key === "left" || key === "right") {
    const final = ARROWS[key];
    if (modified) return `\x1b[1;${param}${final}`;
    return applicationCursor ? `\x1bO${final}` : `\x1b[${final}`;
  }
  if (key === "home" || key === "end") {
    const final = HOME_END[key];
    if (modified) return `\x1b[1;${param}${final}`;
    return applicationCursor ? `\x1bO${final}` : `\x1b[${final}`;
  }
  if (key === "ins" || key === "del" || key === "pgup" || key === "pgdn") {
    const code = TILDE[key];
    return modified ? `\x1b[${code};${param}~` : `\x1b[${code}~`;
  }
  if (key === "f1" || key === "f2" || key === "f3" || key === "f4") {
    const final = F_SS3[key];
    return modified ? `\x1b[1;${param}${final}` : `\x1bO${final}`;
  }
  const code = F_TILDE[key];
  return modified ? `\x1b[${code};${param}~` : `\x1b[${code}~`;
}

/** What typing `data` means while Ctrl is armed: the control character for one
 *  letter or one of `@ [ \ ] ^ _ ?`, a NUL for space. `null` for anything else
 *  (pasted text, an arrow's escape sequence): it goes through unchanged. */
export function applyCtrl(data: string): string | null {
  if ([...data].length !== 1) return null;
  if (data === " ") return "\0";
  if (data === "?") return "\x7f";
  const code = data.toUpperCase().charCodeAt(0);
  if (code >= 0x40 && code <= 0x5f) return String.fromCharCode(code - 0x40);
  return null;
}

/** Typed text under the armed modifiers: Ctrl turns one character into its
 *  control code, Alt prefixes ESC (Meta). `null` when the modifiers change
 *  nothing, so the text goes through unchanged. Text longer than one
 *  character (a paste, an IME commit) is never modified. */
export function modifyTyped(data: string, mods: Modifiers = NO_MODIFIERS): string | null {
  if (!mods.ctrl && !mods.alt) return null;
  if ([...data].length !== 1) return null;
  let out = data;
  if (mods.ctrl) {
    const control = applyCtrl(data);
    if (control === null) return mods.alt ? `\x1b${data}` : null;
    out = control;
  }
  return mods.alt ? `\x1b${out}` : out;
}

/* Sticky modifiers. One tap arms a modifier for the next key; a quick second
   tap locks it until it is tapped again (like Shift on a phone keyboard); a
   tap on a locked one releases it. */

export type ModifierMode = "off" | "armed" | "locked";
export interface ModifierState {
  mode: ModifierMode;
  /** When the last tap happened (ms), to recognise a double tap. */
  at: number;
}

export const DOUBLE_TAP_MS = 350;
export const MODIFIER_OFF: ModifierState = { mode: "off", at: 0 };

export function tapModifier(state: ModifierState, now: number): ModifierState {
  switch (state.mode) {
    case "off":
      return { mode: "armed", at: now };
    case "armed":
      return now - state.at < DOUBLE_TAP_MS ? { mode: "locked", at: now } : { mode: "off", at: now };
    case "locked":
      return { mode: "off", at: now };
  }
}

/** After a key went out: an armed modifier is used up, a locked one stays. */
export function consumeModifier(state: ModifierState): ModifierState {
  return state.mode === "armed" ? { mode: "off", at: state.at } : state;
}
