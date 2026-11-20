import type { Host } from "@/lib/backend";
import { findSavedHost, parseTarget, type Target } from "@/lib/quickConnect";

export const LINK_SCHEME = "anixops-ssh:";

/** What a link asks for. It never carries options: only a host or an address. */
export type LinkRequest = { kind: "host"; ref: string } | { kind: "target"; target: Target };

/** Reads `anixops-ssh://connect?host=<id or name>` and `anixops-ssh://user@host:port`.
 *  Anything else, including extra query parameters, a path or a fragment, is refused. */
export function parseLink(link: string): LinkRequest | null {
  const text = link.trim();
  if (!text.toLowerCase().startsWith(`${LINK_SCHEME}//`)) return null;
  const rest = text.slice(LINK_SCHEME.length + 2);
  if (rest.includes("#")) return null;
  const connect = /^connect\/?\?(.*)$/i.exec(rest);
  if (connect) {
    const params = new URLSearchParams(connect[1]);
    const keys = [...params.keys()];
    const ref = params.get("host")?.trim();
    if (keys.length !== 1 || keys[0] !== "host" || !ref || /[\u0000-\u001f]/.test(ref)) return null;
    return { kind: "host", ref };
  }
  if (rest.includes("?") || rest.includes("%")) return null;
  const parsed = parseTarget(rest.replace(/\/$/, ""));
  if (!parsed || /\s/.test(rest)) return null;
  return { kind: "target", target: { user: parsed.user, host: parsed.host, port: parsed.port } };
}

export type LinkOutcome =
  /** A saved host: open it. */
  | { action: "open"; host: Host }
  /** An address the user has not saved: ask first. */
  | { action: "confirm"; target: Target }
  /** A name that is neither a host nor an address. */
  | { action: "unknown" };

/** What to do with a request, given the saved hosts. Only a saved host opens without asking. */
export function resolveLink(request: LinkRequest, hosts: Host[]): LinkOutcome {
  if (request.kind === "target") {
    const saved = findSavedHost(hosts, request.target);
    return saved ? { action: "open", host: saved } : { action: "confirm", target: request.target };
  }
  const byId = hosts.find((host) => host.id === request.ref);
  if (byId) return { action: "open", host: byId };
  const wanted = request.ref.toLowerCase();
  const byName = hosts.filter((host) => host.name.toLowerCase() === wanted);
  if (byName.length === 1) return { action: "open", host: byName[0] };
  if (byName.length === 0) {
    const target = parseTarget(request.ref);
    if (target?.explicit) {
      const addressed = { user: target.user, host: target.host, port: target.port };
      const saved = findSavedHost(hosts, addressed);
      return saved ? { action: "open", host: saved } : { action: "confirm", target: addressed };
    }
  }
  return { action: "unknown" };
}
