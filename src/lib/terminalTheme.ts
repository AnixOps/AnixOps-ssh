import type { ISearchDecorationOptions } from "@xterm/addon-search";
import type { ITheme } from "@xterm/xterm";
import { useEffect, useLayoutEffect, useState, type RefObject } from "react";
import { onSettings, settings, type TerminalScheme } from "./settings";

/* The terminal palette comes from the live AnixOps tokens (guidelines/terminal.md),
   so light and dark follow the app and no colour is written here:
   red = --danger, green = --success, yellow = --warning, blue = --accent,
   muted = bright black. Magenta and cyan come from the chart colours, which the
   guidelines keep at 3:1 or more on the card. Surfaces and text are --bg and
   --label-1. */

function rootToken(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

export function isDark(): boolean {
  return getComputedStyle(document.documentElement).colorScheme.includes("dark");
}

/* The terminal keeps its own scheme (Settings > Terminal): dark by default
   whatever the app theme (guidelines/terminal.md, "otherwise default to dark";
   iTerm2, Windows Terminal and Termius), light, or the app's. Its tokens are
   the brand's light or dark set, read from the token stylesheet, so no colour
   is written here either. */

const schemeCache = new Map<"dark" | "light", Record<string, string>>();

/** The brand tokens of one scheme: the `:root` rule (light) or the
 *  `:root[data-theme="dark"]` rule, outside media queries. */
function schemeTokens(kind: "dark" | "light"): Record<string, string> | null {
  const cached = schemeCache.get(kind);
  if (cached) return cached;
  const selector = kind === "dark" ? ':root[data-theme="dark"]' : ":root";
  let found: Record<string, string> | null = null;
  const visit = (rules: CSSRuleList) => {
    for (const rule of Array.from(rules)) {
      if (found) return;
      if (rule instanceof CSSMediaRule) continue;
      if (rule instanceof CSSStyleRule) {
        if (rule.selectorText !== selector || rule.style.getPropertyValue("color-scheme").trim() !== kind) continue;
        const tokens: Record<string, string> = {};
        for (const name of Array.from(rule.style)) if (name.startsWith("--")) tokens[name] = rule.style.getPropertyValue(name).trim();
        found = tokens;
      } else if ("cssRules" in rule) visit((rule as CSSGroupingRule).cssRules);
    }
  };
  for (const sheet of Array.from(document.styleSheets)) {
    try {
      visit(sheet.cssRules);
    } catch {
      // A stylesheet from another origin cannot be read; the tokens are not in one.
    }
    if (found) break;
  }
  if (found) schemeCache.set(kind, found);
  return found;
}

/** The token set the terminal uses, or null to follow the app. */
function overrideTokens(scheme: TerminalScheme): Record<string, string> | null {
  if (scheme === "app") return null;
  // Same as the app: nothing to override.
  if ((scheme === "dark") === isDark()) return null;
  return schemeTokens(scheme);
}

/** Puts the terminal's scheme on `element` (the pane around the terminal),
 *  so its padding, find bar and notices match the terminal. */
export function applyTerminalScheme(element: HTMLElement, scheme: TerminalScheme = settings().terminalScheme) {
  const tokens = overrideTokens(scheme);
  for (const name of Array.from(element.style)) if (name.startsWith("--")) element.style.removeProperty(name);
  element.style.colorScheme = "";
  if (!tokens) return;
  for (const [name, value] of Object.entries(tokens)) element.style.setProperty(name, value);
  element.style.colorScheme = scheme;
}

/** Keeps `element` in the terminal's scheme as the setting or the app theme change. */
export function useTerminalScheme(ref: RefObject<HTMLElement | null>, scheme?: TerminalScheme) {
  const [setting, setSetting] = useState(() => settings().terminalScheme);
  const [, setTick] = useState(0);
  useEffect(() => onSettings((next) => {
    setSetting(next.terminalScheme);
    setTick((tick) => tick + 1);
  }), []);
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const bump = () => setTick((tick) => tick + 1);
    media.addEventListener("change", bump);
    return () => media.removeEventListener("change", bump);
  }, []);
  useLayoutEffect(() => {
    if (ref.current) applyTerminalScheme(ref.current, scheme ?? setting);
  });
}

export function terminalTheme(scheme: TerminalScheme = settings().terminalScheme): ITheme {
  const tokens = overrideTokens(scheme);
  const dark = tokens ? scheme === "dark" : isDark();
  const token = (name: string) => tokens?.[name] ?? rootToken(name);
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
export function searchDecorations(scheme: TerminalScheme = settings().terminalScheme): ISearchDecorationOptions {
  const tokens = overrideTokens(scheme);
  const match = tokens?.["--warning"] ?? rootToken("--warning");
  const active = tokens?.["--accent"] ?? rootToken("--accent");
  return {
    matchBackground: match,
    matchOverviewRuler: match,
    activeMatchBackground: active,
    activeMatchColorOverviewRuler: active,
  };
}

export function terminalFont(): string {
  return rootToken("--font-app-mono") || "monospace";
}
