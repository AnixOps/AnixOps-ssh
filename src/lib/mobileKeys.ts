/* What the on-screen key bar sends. A soft keyboard has no Esc, Tab, Ctrl or
   arrows; xterm.js cannot produce them from touch, so the bar writes the same
   bytes a hardware keyboard would. */

export type BarKey = "esc" | "tab" | "up" | "down" | "left" | "right";

const ARROWS: Record<"up" | "down" | "left" | "right", string> = {
  up: "A",
  down: "B",
  right: "C",
  left: "D",
};

/** The bytes for a bar key. Arrows follow the terminal's cursor-key mode
 *  (full-screen programs such as vim and less switch it on) and, with Ctrl
 *  armed, carry the Ctrl modifier the way xterm does (`ESC [ 1 ; 5 A`). */
export function keySequence(key: BarKey, applicationCursor: boolean, ctrl: boolean): string {
  if (key === "esc") return "\x1b";
  if (key === "tab") return "\t";
  const final = ARROWS[key];
  if (ctrl) return `\x1b[1;5${final}`;
  return applicationCursor ? `\x1bO${final}` : `\x1b[${final}`;
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
