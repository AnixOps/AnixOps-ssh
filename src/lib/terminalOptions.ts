import type { ITerminalOptions, ITheme } from "@xterm/xterm";
import type { Settings } from "./settings";

/** The parts of the settings the terminal reads. */
export type TerminalSettings = Pick<
  Settings,
  | "terminalFontSize"
  | "terminalScrollback"
  | "terminalOptionAsMeta"
  | "terminalFontFamily"
  | "terminalLineHeight"
  | "terminalCursorStyle"
  | "terminalCursorBlink"
>;

/** The chosen font in front of the built-in stack, which stays as the fallback.
 *  A bare name is quoted; a list the person typed (commas or quotes) is kept. */
export function resolveFontFamily(chosen: string, fallback: string): string {
  const name = chosen.trim();
  if (!name) return fallback;
  const list = /[,"']/.test(name) ? name : `"${name}"`;
  return `${list}, ${fallback}`;
}

/** What differs per view: the font, the palette, and the size the view shows. */
export interface TerminalOptionsInput {
  fontFamily: string;
  theme: ITheme;
  /** The view's font size; the desktop uses the setting, the phone its own. */
  fontSize?: number;
  screenReaderMode?: boolean;
}

/** Options shared by the desktop and the touch terminal, so the two behave alike. */
export function terminalOptions(current: TerminalSettings, input: TerminalOptionsInput): ITerminalOptions {
  return {
    fontFamily: resolveFontFamily(current.terminalFontFamily, input.fontFamily),
    fontSize: input.fontSize ?? current.terminalFontSize,
    lineHeight: current.terminalLineHeight,
    cursorStyle: current.terminalCursorStyle,
    cursorBlink: current.terminalCursorBlink,
    scrollback: current.terminalScrollback,
    // Needed by the Unicode 11 width tables and shell integration.
    allowProposedApi: true,
    // Off by default: Option is how @ | [ ] { } ~ € are typed on most non-US
    // Mac layouts. Users who want Option as Meta (Emacs, readline) turn it on.
    macOptionIsMeta: current.terminalOptionAsMeta,
    // Option-click selects even when a full-screen app has the mouse.
    macOptionClickForcesSelection: true,
    screenReaderMode: input.screenReaderMode ?? false,
    theme: input.theme,
  };
}

/** The options that follow a settings change on a live terminal. */
export function liveTerminalOptions(
  current: TerminalSettings,
  fallbackFont: string,
): Pick<
  ITerminalOptions,
  "scrollback" | "macOptionIsMeta" | "fontFamily" | "lineHeight" | "cursorStyle" | "cursorBlink"
> {
  return {
    scrollback: current.terminalScrollback,
    macOptionIsMeta: current.terminalOptionAsMeta,
    fontFamily: resolveFontFamily(current.terminalFontFamily, fallbackFont),
    lineHeight: current.terminalLineHeight,
    cursorStyle: current.terminalCursorStyle,
    cursorBlink: current.terminalCursorBlink,
  };
}

/** Wide characters (CJK, emoji) take two cells, as current terminals and `wcwidth` count them. */
export const UNICODE_VERSION = "11";
