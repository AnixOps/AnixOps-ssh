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
