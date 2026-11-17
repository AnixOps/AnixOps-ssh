/** Which suggested commands may be put on the terminal's input line. */

const MAX_COMMAND_CHARS = 2000;
// Control characters, and invisible or direction-changing characters that
// can make a command read differently from what it does.
// eslint-disable-next-line no-control-regex
const UNSAFE = /[\u0000-\u001f\u007f-\u009f​-‏‪-‮⁠-⁩﻿]/;

/**
 * The text to place on the input line for a code block from the answer, or
 * `null` when the block must not be inserted: more than one line (a newline
 * could run part of it), control or invisible characters, or too long. A
 * leading shell prompt (`$ `) is removed. Inserting never presses Enter; this
 * only decides what the user gets to edit and run themselves.
 */
export function insertableCommand(code: string): string | null {
  const text = code.replace(/\r\n/g, "\n").replace(/\s+$/, "").replace(/^\s*\n/, "");
  if (text === "" || text.includes("\n")) return null;
  const command = text.replace(/^\$(?:\s+|$)/, "").trim();
  if (command === "" || command.length > MAX_COMMAND_CHARS || UNSAFE.test(command)) return null;
  return command;
}
