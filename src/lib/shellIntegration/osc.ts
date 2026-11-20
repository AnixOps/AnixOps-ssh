/**
 * The OSC sequence of the opt-in shell integration (see
 * shell-integration/README.md): `OSC 6973 ; <kind> ; <payload> ST`.
 * xterm hands the handler what follows `6973;`.
 */
export const INTEGRATION_OSC = 6973;

export type IntegrationEvent =
  /** The script is loaded; `shell` is bash, zsh or fish. */
  | { kind: "ready"; shell: string }
  /** The prompt starts (this is where a mark belongs). */
  | { kind: "promptStart" }
  /** The prompt ends: what the user types begins here. */
  | { kind: "promptEnd" }
  /** A command was entered; `command` is the exact line the shell has. */
  | { kind: "commandStart"; command: string }
  /** The command finished. */
  | { kind: "commandEnd"; exitCode: number | null }
  /** The shell is in this directory. */
  | { kind: "cwd"; path: string };

/** Decodes `%XX` escapes as UTF-8; malformed escapes stay as they are. */
export function percentDecode(text: string): string {
  const bytes: number[] = [];
  const encoder = new TextEncoder();
  for (let i = 0; i < text.length; ) {
    if (text[i] === "%" && /^[0-9a-fA-F]{2}$/.test(text.slice(i + 1, i + 3))) {
      bytes.push(Number.parseInt(text.slice(i + 1, i + 3), 16));
      i += 3;
    } else {
      const code = text.codePointAt(i) as number;
      const char = String.fromCodePoint(code);
      bytes.push(...encoder.encode(char));
      i += char.length;
    }
  }
  return new TextDecoder("utf-8").decode(Uint8Array.from(bytes));
}

/** Parses the data of one sequence; null for kinds this app does not know. */
export function parseIntegrationOsc(data: string): IntegrationEvent | null {
  const separator = data.indexOf(";");
  const kind = separator < 0 ? data : data.slice(0, separator);
  const payload = separator < 0 ? "" : data.slice(separator + 1);
  switch (kind) {
    case "R":
      return { kind: "ready", shell: payload };
    case "A":
      return { kind: "promptStart" };
    case "B":
      return { kind: "promptEnd" };
    case "C":
      return { kind: "commandStart", command: percentDecode(payload) };
    case "D": {
      const code = /^-?\d{1,5}$/.test(payload) ? Number(payload) : null;
      return { kind: "commandEnd", exitCode: code };
    }
    case "P":
      return payload === "" ? null : { kind: "cwd", path: percentDecode(payload) };
    default:
      return null;
  }
}

/** The folder in an OSC 7 report (`OSC 7 ; file://host/path ST`), which
 *  many shells send without the integration script: Fedora's and Ubuntu's
 *  VTE profile, fish, zsh frameworks, and what Windows Terminal, WezTerm
 *  and iTerm2 read. xterm hands over what follows `7;`. The host part is
 *  ignored (it names the server the shell runs on). */
export function parseOsc7(data: string): string | null {
  const match = /^file:\/\/[^/]*(\/.*)$/.exec(data.trim());
  if (!match) return null;
  const path = percentDecode(match[1]);
  const control = [...path].some((char) => char.charCodeAt(0) < 0x20 || char === "\u007f");
  return path.startsWith("/") && !control ? path : null;
}
