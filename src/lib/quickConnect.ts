import type { Host } from "@/lib/backend";

/** An SSH destination that is typed or linked rather than saved. */
export interface Target {
  user: string | null;
  host: string;
  port: number;
}

export interface QuickTarget extends Target {
  /** False for a bare single word like `web`, which is as likely a search. */
  explicit: boolean;
}

export const ADHOC_PREFIX = "adhoc:";

// The same rules as validate_username and validate_host in easyssh-core, a bit
// stricter for the host: a name or an address, nothing a shell would read.
const USER = /^[A-Za-z0-9_][A-Za-z0-9_-]*$/;
const NAME = /^[A-Za-z0-9_]([A-Za-z0-9_.-]*[A-Za-z0-9_])?$/;
const V6 = /^[0-9A-Fa-f:.]+$/;

function parsePort(text: string): number | null {
  if (!/^[0-9]{1,5}$/.test(text)) return null;
  const port = Number(text);
  return port >= 1 && port <= 65535 ? port : null;
}

function isV6(host: string): boolean {
  return V6.test(host) && host.split(":").length >= 3;
}

/** `[user@]host[:port]`, with an IPv6 address in brackets (or bare, with no port). */
function parseAuthority(text: string): { user: string | null; host: string; port: number | null; v6: boolean } | null {
  if (!text || /\s/.test(text)) return null;
  const at = text.lastIndexOf("@");
  const user = at >= 0 ? text.slice(0, at) : null;
  const rest = at >= 0 ? text.slice(at + 1) : text;
  if (user !== null && !USER.test(user)) return null;
  let host: string;
  let port: number | null = null;
  if (rest.startsWith("[")) {
    const close = rest.indexOf("]");
    if (close < 0) return null;
    host = rest.slice(1, close);
    const tail = rest.slice(close + 1);
    if (tail) {
      if (!tail.startsWith(":")) return null;
      port = parsePort(tail.slice(1));
      if (port === null) return null;
    }
    return isV6(host) ? { user, host, port, v6: true } : null;
  }
  if (isV6(rest)) return { user, host: rest, port: null, v6: true };
  const colon = rest.lastIndexOf(":");
  if (colon >= 0) {
    host = rest.slice(0, colon);
    port = parsePort(rest.slice(colon + 1));
    if (port === null) return null;
  } else {
    host = rest;
  }
  return NAME.test(host) ? { user, host, port, v6: false } : null;
}

/** `ssh [-p port] [-l user] [user@]host`; any other option is refused. */
function parseCommand(text: string): QuickTarget | null {
  const words = text.split(/\s+/);
  let port: number | null = null;
  let login: string | null = null;
  let destination: string | null = null;
  for (let i = 1; i < words.length; i++) {
    const word = words[i];
    const value = (flag: string): string | null => {
      if (word === flag) return words[++i] ?? null;
      return word.slice(flag.length) || null;
    };
    if (word.startsWith("-p")) {
      port = parsePort(value("-p") ?? "");
      if (port === null) return null;
    } else if (word.startsWith("-l")) {
      login = value("-l");
      if (login === null || !USER.test(login)) return null;
    } else if (word.startsWith("-")) return null;
    else if (destination === null) destination = word;
    else return null;
  }
  if (destination === null) return null;
  const parsed = parseAuthority(destination);
  if (!parsed) return null;
  if (parsed.port !== null && port !== null && parsed.port !== port) return null;
  if (parsed.user !== null && login !== null && parsed.user !== login) return null;
  return { user: parsed.user ?? login, host: parsed.host, port: parsed.port ?? port ?? 22, explicit: true };
}

/** Reads what a user typed or pasted as an SSH destination; null when it is not one. */
export function parseTarget(input: string): QuickTarget | null {
  const text = input.trim();
  if (!text) return null;
  if (/^ssh\s/.test(text)) return parseCommand(text);
  const scheme = /^ssh:\/\//i.exec(text);
  const body = scheme ? text.slice(scheme[0].length).replace(/\/$/, "") : text;
  const parsed = parseAuthority(body);
  if (!parsed) return null;
  const explicit = Boolean(scheme) || parsed.user !== null || parsed.port !== null || parsed.v6 || /[.]/.test(parsed.host) || parsed.host === "localhost";
  return { user: parsed.user, host: parsed.host, port: parsed.port ?? 22, explicit };
}

/** `user@host:port` as shown to people: no port when it is 22, brackets for IPv6. */
export function formatTarget(target: Target): string {
  const host = target.host.includes(":") ? `[${target.host}]` : target.host;
  return `${target.user ? `${target.user}@` : ""}${host}${target.port === 22 ? "" : `:${target.port}`}`;
}

/** The id the backend opens a session for, without a saved host. */
export function adhocId(target: Target): string {
  const host = target.host.includes(":") ? `[${target.host}]` : target.host;
  return `${ADHOC_PREFIX}${target.user ? `${target.user}@` : ""}${host}:${target.port}`;
}

/** The target an `adhoc:` id stands for; null for any other id. */
export function parseAdhocId(id: string): Target | null {
  if (!id.startsWith(ADHOC_PREFIX)) return null;
  const parsed = parseTarget(id.slice(ADHOC_PREFIX.length));
  return parsed && { user: parsed.user, host: parsed.host, port: parsed.port };
}

/** The saved host this target already is, if any: same address and port, and the same user when one is given. */
export function findSavedHost(hosts: Host[], target: Target): Host | undefined {
  const wanted = target.host.toLowerCase();
  return hosts.find((host) => {
    const { alias, hostname, username, port, serial } = host.fields;
    if (serial) return false;
    const name = (alias ? host.name : hostname).toLowerCase();
    if (name !== wanted && hostname.toLowerCase() !== wanted) return false;
    if (alias) return target.user === null && target.port === 22;
    return (port || 22) === target.port && (target.user === null || username === target.user);
  });
}
