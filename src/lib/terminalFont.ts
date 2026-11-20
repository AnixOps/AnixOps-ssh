import type { Settings } from "./settings";

/** Font choices of the terminal that live apart from the font size. An empty
 *  family means "the default stack for this system". */
export interface TerminalFontSettings {
  family: string;
  lineHeight: number;
  /** Extra space between cells, in CSS pixels. */
  letterSpacing: number;
}

export const FONT_SIZE_MIN = 9;
export const FONT_SIZE_MAX = 32;
export const LINE_HEIGHT_MIN = 1;
export const LINE_HEIGHT_MAX = 2;
export const LETTER_SPACING_MIN = -2;
export const LETTER_SPACING_MAX = 10;

export const defaultTerminalFont: TerminalFontSettings = { family: "", lineHeight: 1.2, letterSpacing: 0 };

export type FontPlatform = "mac" | "windows" | "linux";

/** The first installed font wins; every stack ends in the generic `monospace`. */
const STACKS: Record<FontPlatform, string> = {
  mac: '"SF Mono", Menlo, Monaco, "Courier New", monospace',
  windows: '"Cascadia Mono", Consolas, "Courier New", monospace',
  linux: '"DejaVu Sans Mono", "Liberation Mono", "Ubuntu Mono", monospace',
};

export function platformOf(platform: string): FontPlatform {
  if (/Mac|iPhone|iPad/.test(platform)) return "mac";
  if (/Win/.test(platform)) return "windows";
  return "linux";
}

export function defaultFontStack(platform: FontPlatform): string {
  return STACKS[platform];
}

/** Common monospace fonts offered as suggestions; the field also takes any name. */
export const SUGGESTED_FONTS = [
  "JetBrains Mono",
  "Fira Code",
  "Cascadia Code",
  "Source Code Pro",
  "Hack",
  "Menlo",
  "Consolas",
  "SF Mono",
  "DejaVu Sans Mono",
];

/** A family list safe to hand to CSS: no control characters and no characters
 *  that could end the declaration. Returns "" when nothing usable is left. */
export function sanitizeFamily(input: string): string {
  // eslint-disable-next-line no-control-regex
  return input.replace(/[\u0000-\u001f\u007f;{}<>\\]/g, "").trim().slice(0, 300);
}

/** Quote a bare family name that contains spaces; leave lists and quoted names alone. */
function normaliseFamily(family: string): string {
  if (/[,"']/.test(family)) return family;
  return /\s/.test(family) ? `"${family}"` : family;
}

/** The CSS font-family for the terminal: the user's choice first, then the
 *  system stack, so a missing font falls back instead of showing a proportional face. */
export function terminalFontFamily(font: TerminalFontSettings, platform: FontPlatform, tokenFont: string): string {
  const custom = sanitizeFamily(font.family);
  const fallback = tokenFont || defaultFontStack(platform);
  return custom ? `${normaliseFamily(custom)}, ${fallback}` : fallback;
}

export function clamp(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, value));
}

export function clampFont(font: TerminalFontSettings): TerminalFontSettings {
  return {
    family: sanitizeFamily(font.family),
    lineHeight: Math.round(clamp(font.lineHeight, LINE_HEIGHT_MIN, LINE_HEIGHT_MAX) * 100) / 100,
    letterSpacing: Math.round(clamp(font.letterSpacing, LETTER_SPACING_MIN, LETTER_SPACING_MAX)),
  };
}

/** The next font size for a zoom command, kept inside the range the settings accept. */
export function zoomedSize(size: number, command: "zoom-in" | "zoom-out" | "zoom-reset", defaultSize = 13): number {
  if (command === "zoom-reset") return defaultSize;
  return clamp(size + (command === "zoom-in" ? 1 : -1), FONT_SIZE_MIN, FONT_SIZE_MAX);
}

/** The xterm options that follow the font settings. */
export function fontOptions(
  current: Pick<Settings, "terminalFontSize" | "terminalFont">,
  platform: FontPlatform,
  tokenFont: string,
) {
  const font = clampFont(current.terminalFont ?? defaultTerminalFont);
  return {
    fontFamily: terminalFontFamily(font, platform, tokenFont),
    fontSize: current.terminalFontSize,
    lineHeight: font.lineHeight,
    letterSpacing: font.letterSpacing,
  };
}
