/** Unix permission bits for the file pane's Permissions dialog. */

export const PERMISSION_BITS = [
  { key: "ur", bit: 0o400 },
  { key: "uw", bit: 0o200 },
  { key: "ux", bit: 0o100 },
  { key: "gr", bit: 0o040 },
  { key: "gw", bit: 0o020 },
  { key: "gx", bit: 0o010 },
  { key: "or", bit: 0o004 },
  { key: "ow", bit: 0o002 },
  { key: "ox", bit: 0o001 },
] as const;

/** The mode in an `ls -l` string such as `-rwxr-xr--` or `drwxr-sr-t`; null when it is not one. */
export function modeFromListing(text: string): number | null {
  if (text.length < 10 || !/^[-dlcbpsD?][-rwxsStT]{9}/.test(text)) return null;
  let mode = 0;
  for (let index = 0; index < 9; index += 1) {
    const ch = text[index + 1];
    if (ch === "-") continue;
    if (index === 2 && (ch === "s" || ch === "S")) mode |= 0o4000;
    if (index === 5 && (ch === "s" || ch === "S")) mode |= 0o2000;
    if (index === 8 && (ch === "t" || ch === "T")) mode |= 0o1000;
    if (ch !== "S" && ch !== "T") mode |= 1 << (8 - index);
  }
  return mode;
}

/** `0755` style text of a mode. */
export function formatOctal(mode: number): string {
  return (mode & 0o7777).toString(8).padStart(3, "0");
}

/** The mode of octal text of 3 or 4 digits; null when it is not valid. */
export function parseOctal(text: string): number | null {
  const trimmed = text.trim();
  if (!/^[0-7]{3,4}$/.test(trimmed)) return null;
  return Number.parseInt(trimmed, 8);
}

/** `rwxr-xr-x` for the nine permission bits of `mode`. */
export function formatSymbolic(mode: number): string {
  return "rwxrwxrwx"
    .split("")
    .map((symbol, index) => (mode & (1 << (8 - index)) ? symbol : "-"))
    .join("");
}

export function toggleBit(mode: number, bit: number): number {
  return mode ^ bit;
}
