/* Pure rules for what the remote side may make the terminal do, and what a
   click in it does: links, OSC 52 clipboard writes and window titles. */

/** Only web links open, and only with Ctrl-click (⌘-click on macOS), so a
 *  plain click still selects text and a remote program cannot open anything. */
export function shouldOpenLink(
  uri: string,
  event: Pick<MouseEvent, "ctrlKey" | "metaKey">,
  mac: boolean,
): boolean {
  if (!(mac ? event.metaKey : event.ctrlKey)) return false;
  try {
    const url = new URL(uri);
    return (url.protocol === "http:" || url.protocol === "https:") && url.hostname !== "";
  } catch {
    return false;
  }
}

/** The largest text an OSC 52 write may put on the clipboard (1 MB, as
 *  xterm's and tmux's limits are of that order). */
export const OSC52_MAX_BYTES = 1024 * 1024;

/** The text of an OSC 52 clipboard write (`ESC ] 52 ; c ; <base64> BEL`;
 *  xterm.js hands over what follows `52;`). Only writes are honoured: a `?`
 *  query, which would let the remote side read the local clipboard, an
 *  empty payload, bad base64 or more than OSC52_MAX_BYTES give `null`. */
export function parseOsc52(data: string): string | null {
  const split = data.indexOf(";");
  if (split < 0) return null;
  const targets = data.slice(0, split);
  const payload = data.slice(split + 1);
  if (!/^[cpqs0-7]*$/.test(targets)) return null;
  if (payload === "" || payload === "?") return null;
  // Base64 grows by a third; refuse before decoding anything large.
  if (payload.length > Math.ceil(OSC52_MAX_BYTES / 3) * 4) return null;
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(payload)) return null;
  let binary: string;
  try {
    binary = atob(payload);
  } catch {
    return null;
  }
  const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
  if (bytes.length > OSC52_MAX_BYTES) return null;
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

/** The longest remote title shown. */
export const TITLE_MAX = 120;

/** A remote window title (OSC 0/2) made safe to show: control and bidi
 *  override characters removed, whitespace collapsed, at most TITLE_MAX
 *  characters. */
export function sanitizeTitle(title: string): string {
  const clean = title
    // C0, DEL, C1, and the bidi controls that could reorder the text around it.
    .replace(/[\u0000-\u001f\u007f-\u009f‎‏‪-‮⁦-⁩]/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  const chars = [...clean];
  return chars.length > TITLE_MAX ? chars.slice(0, TITLE_MAX - 1).join("") + "…" : clean;
}

/** Capabilities answered to XTGETTCAP (`DCS + q <hex names> ST`). Programs
 *  ask for these when the environment says little: `COLORTERM` rarely
 *  survives SSH (servers accept only `LANG` and `LC_*`), so Neovim and others
 *  query `Tc`/`RGB` to learn that 24-bit colour works. `true` is a boolean
 *  capability; strings are terminfo values. */
const TERMINAL_CAPABILITIES: Record<string, string | true> = {
  Tc: true,
  RGB: true,
  setrgbf: "\\E[38;2;%p1%d;%p2%d;%p3%dm",
  setrgbb: "\\E[48;2;%p1%d;%p2%d;%p3%dm",
  // Underline styles (undercurl) and colours.
  Smulx: "\\E[4:%p1%dm",
  Setulc: "\\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m",
  // OSC 52 clipboard writes (tmux's set-clipboard).
  Ms: "\\E]52;%p1%s;%p2%s\\007",
  // Cursor shape.
  Ss: "\\E[%p1%d q",
  Se: "\\E[2 q",
  TN: "xterm-256color",
  colors: "256",
};

function hexToText(hex: string): string | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) return null;
  let out = "";
  for (let i = 0; i < hex.length; i += 2) out += String.fromCharCode(parseInt(hex.slice(i, i + 2), 16));
  return out;
}

function textToHex(text: string): string {
  return Array.from(text, (char) => char.charCodeAt(0).toString(16).padStart(2, "0").toUpperCase()).join("");
}

/** The reply to an XTGETTCAP request (the DCS payload, hex-encoded names
 *  separated by `;`), as xterm gives it: `DCS 1 + r name=value ST` for each
 *  known name, `DCS 0 + r name ST` for an unknown one. */
export function capabilityReply(payload: string): string {
  return payload
    .split(";")
    .filter((hex) => hex !== "")
    .slice(0, 16)
    .map((hex) => {
      const name = hexToText(hex);
      const value = name === null ? undefined : TERMINAL_CAPABILITIES[name];
      if (value === undefined) return `\x1bP0+r${hex}\x1b\\`;
      const head = `\x1bP1+r${hex.toUpperCase()}`;
      return value === true ? `${head}\x1b\\` : `${head}=${textToHex(value.replace(/\\E/g, "\x1b"))}\x1b\\`;
    })
    .join("");
}

/** DEC private modes that turn mouse tracking on (X10, normal, button-event,
 *  any-event). The encodings (1006 SGR and the like) only change the format. */
export const MOUSE_TRACKING_MODES: readonly number[] = [9, 1000, 1002, 1003];

/** What a `CSI ? Pm h` or `l` does to mouse tracking: the mode it names
 *  last (`null` when none), and whether it names nothing else, so it can be
 *  dropped as a whole when programs may not use the mouse. */
export function mouseTrackingRequest(params: readonly (number | number[])[]): {
  mode: number | null;
  only: boolean;
} {
  const modes = params.map((param) => (typeof param === "number" ? param : param[0]));
  const tracking = modes.filter((mode) => MOUSE_TRACKING_MODES.includes(mode));
  return {
    mode: tracking.length > 0 ? tracking[tracking.length - 1] : null,
    only: tracking.length > 0 && tracking.length === modes.length,
  };
}
