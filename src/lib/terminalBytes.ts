/* Terminal output reaches the web view as text, but a ZMODEM transfer is
   binary. The engine therefore never loses a byte: one that cannot be part of
   UTF-8 is carried as the private-use character U+100000 + byte (U+100080 to
   U+1000FF for 0x80 to 0xFF; Supplementary Private Use Area-B, outside what
   Nerd Fonts use; JS holds them as surrogate pairs, so strings are walked by
   code point), and the same mapping runs backwards on input
   (crates/easyssh-engine/src/utf8.rs). A plain terminal shows those as
   U+FFFD, as it always did; a transfer turns the text back into bytes. */

const ESCAPE_BASE = 0x100000;
const ESCAPE_FIRST = 0x100080;
const ESCAPE_LAST = 0x1000ff;

const encoder = new TextEncoder();

/** The text as the bytes the remote side sent (inverse of the engine's decoding). */
export function textToBytes(text: string): Uint8Array {
  let plain = true;
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i);
    if (code >= 0x80) {
      plain = false;
      break;
    }
  }
  if (plain) {
    const out = new Uint8Array(text.length);
    for (let i = 0; i < text.length; i += 1) out[i] = text.charCodeAt(i);
    return out;
  }
  const parts: Uint8Array[] = [];
  let run = "";
  const flush = () => {
    if (run) parts.push(encoder.encode(run));
    run = "";
  };
  for (const char of text) {
    const code = char.codePointAt(0) ?? 0;
    if (code >= ESCAPE_FIRST && code <= ESCAPE_LAST) {
      flush();
      parts.push(Uint8Array.of(code - ESCAPE_BASE));
    } else {
      run += char;
    }
  }
  flush();
  const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

/** Bytes as text for `session.write`: ASCII as it is, everything above as an escaped byte. */
export function bytesToText(bytes: ArrayLike<number>): string {
  let out = "";
  const step = 8192;
  for (let from = 0; from < bytes.length; from += step) {
    const codes: number[] = [];
    const end = Math.min(from + step, bytes.length);
    for (let i = from; i < end; i += 1) {
      const byte = bytes[i];
      codes.push(byte < 0x80 ? byte : ESCAPE_BASE + byte);
    }
    out += String.fromCodePoint(...codes);
  }
  return out;
}

/** Output for xterm: escaped bytes (undecodable garbage) show as U+FFFD. */
export function displayText(text: string): string {
  return text.replace(/[\u{100080}-\u{1000ff}]/gu, "\ufffd");
}

/** The start of a ZMODEM hex header: ZPAD ZPAD ZDLE 'B', then type 00 (ZRQINIT,
 *  `sz` is sending) or 01 (ZRINIT, `rz` is waiting). */
export const ZMODEM_START = /\*\*\x18B0[01]/;

/** How many characters before a chunk's end could be the start of a handshake
 *  that finishes in the next chunk. */
export const ZMODEM_LOOKBEHIND = 5;
