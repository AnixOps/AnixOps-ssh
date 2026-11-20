/**
 * Unix permission bits for the Properties dialog: reading the mode from an
 * `ls -l` string, the octal text field, and the rwx checkboxes.
 */

export type Who = "user" | "group" | "other";
export type Bit = "read" | "write" | "execute";

export const WHO: Who[] = ["user", "group", "other"];
export const BITS: Bit[] = ["read", "write", "execute"];

const SHIFT: Record<Who, number> = { user: 6, group: 3, other: 0 };
const VALUE: Record<Bit, number> = { read: 4, write: 2, execute: 1 };

/** The largest mode: setuid, setgid, sticky and rwx for all. */
export const MAX_MODE = 0o7777;

/** Whether `who` has `bit` in `mode`. */
export function hasBit(mode: number, who: Who, bit: Bit): boolean {
  return (mode & (VALUE[bit] << SHIFT[who])) !== 0;
}

/** `mode` with `bit` of `who` switched on or off. */
export function setBit(mode: number, who: Who, bit: Bit, on: boolean): number {
  const mask = VALUE[bit] << SHIFT[who];
  return on ? mode | mask : mode & ~mask;
}

/**
 * The mode in an `ls -l` string such as `-rwxr-xr-x` or `drwxrwxrwt`, with
 * setuid, setgid and sticky read from `s`/`S`/`t`/`T`. Null when the text is
 * not a mode string.
 */
export function parseModeString(text: string): number | null {
  if (!/^[-dlcbpsD?][-rwxsStT]{9}$/.test(text)) return null;
  let mode = 0;
  for (let index = 0; index < 9; index += 1) {
    const symbol = text[index + 1];
    const bit = 1 << (8 - index);
    if (symbol === "r" || symbol === "w" || symbol === "x") mode |= bit;
    else if (symbol === "s" || symbol === "t") mode |= bit | (index === 8 ? 0o1000 : index === 5 ? 0o2000 : 0o4000);
    else if (symbol === "S" || symbol === "T") mode |= index === 8 ? 0o1000 : index === 5 ? 0o2000 : 0o4000;
  }
  return mode;
}

/** Octal text: three digits, or four when setuid, setgid or sticky is set. */
export function formatOctal(mode: number): string {
  const bits = mode & MAX_MODE;
  const digits = bits.toString(8);
  return digits.length < 3 ? digits.padStart(3, "0") : digits;
}

/** The mode typed as octal (`644`, `0755`, `4755`), or null when it is not one. */
export function parseOctal(text: string): number | null {
  const trimmed = text.trim();
  if (!/^[0-7]{1,4}$/.test(trimmed)) return null;
  return Number.parseInt(trimmed, 8);
}

/** `rwxr-xr-x`: the nine permission columns, with s/S/t/T for the special bits. */
export function formatSymbolic(mode: number): string {
  let text = "";
  for (const who of WHO) {
    for (const bit of BITS) {
      const set = hasBit(mode, who, bit);
      const special = bit === "execute" ? (who === "user" ? 0o4000 : who === "group" ? 0o2000 : 0o1000) : 0;
      if (special && mode & special) {
        const letter = who === "other" ? "t" : "s";
        text += set ? letter : letter.toUpperCase();
      } else text += set ? { read: "r", write: "w", execute: "x" }[bit] : "-";
    }
  }
  return text;
}
