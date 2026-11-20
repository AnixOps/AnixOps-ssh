import type { ISearchDecorationOptions } from "@xterm/addon-search";
import type { ITheme } from "@xterm/xterm";

/* The terminal palette comes from the live AnixOps tokens (guidelines/terminal.md),
   so light and dark follow the app and no colour is written here:
   red = --danger, green = --success, yellow = --warning, blue = --accent,
   muted = bright black. Magenta and cyan come from the chart colours, which the
   guidelines keep at 3:1 or more on the card. Surfaces and text are --bg and
   --label-1. */

function token(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

export function isDark(): boolean {
  return getComputedStyle(document.documentElement).colorScheme.includes("dark");
}

export function terminalTheme(): ITheme {
  const dark = isDark();
  return {
    // ANSI black is dark in both schemes, as in every common terminal theme:
    // TUIs (htop, tmux, mc) put it on coloured bars as the text colour.
    black: token(dark ? "--bg-elevated" : "--label-1"),
    red: token("--danger"),
    green: token("--success"),
    yellow: token("--warning"),
    blue: token("--accent"),
    magenta: token("--chart-3"),
    cyan: token("--chart-2"),
    white: token(dark ? "--label-2" : "--label-3"),
    // Muted: hints, timestamps, autosuggestions.
    brightBlack: token(dark ? "--label-3" : "--label-2"),
    brightRed: token("--danger"),
    brightGreen: token("--chart-7"),
    brightYellow: token("--chart-6"),
    brightBlue: token("--accent-hover"),
    brightMagenta: token("--chart-4"),
    brightCyan: token("--chart-2"),
    brightWhite: token("--label-1"),
    background: token("--bg"),
    foreground: token("--label-1"),
    cursor: token("--accent"),
    cursorAccent: token("--bg"),
    selectionBackground: token("--accent-soft"),
  };
}

/** Find-bar match highlights; the search addon needs #RRGGBB values, which the tokens are. */
export function searchDecorations(): ISearchDecorationOptions {
  const match = token("--warning");
  const active = token("--accent");
  return {
    matchBackground: match,
    matchOverviewRuler: match,
    activeMatchBackground: active,
    activeMatchColorOverviewRuler: active,
  };
}

export function terminalFont(): string {
  return token("--font-app-mono") || "monospace";
}
