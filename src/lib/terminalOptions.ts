import type { ITerminalOptions, ITheme } from "@xterm/xterm";
import type { Settings } from "./settings";

/** The parts of the settings the terminal reads. */
export type TerminalSettings = Pick<Settings, "terminalFontSize" | "terminalScrollback" | "terminalOptionAsMeta">;

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
    fontFamily: input.fontFamily,
    fontSize: input.fontSize ?? current.terminalFontSize,
    lineHeight: 1.2,
    cursorBlink: true,
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
export function liveTerminalOptions(current: TerminalSettings): Pick<ITerminalOptions, "scrollback" | "macOptionIsMeta"> {
  return { scrollback: current.terminalScrollback, macOptionIsMeta: current.terminalOptionAsMeta };
}

/** Wide characters (CJK, emoji) take two cells, as current terminals and `wcwidth` count them. */
export const UNICODE_VERSION = "11";
