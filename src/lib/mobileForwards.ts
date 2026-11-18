import type { Tunnel, TunnelInput } from "./backend";

/** Pure logic of the phone's port-forward form. A phone offers local forwards
 *  (`ssh -L`): a port on the phone that reaches a target through a host. Remote
 *  forwards and SOCKS proxies stay on the desktop. */

export interface ForwardDraft {
  name: string;
  hostId: string;
  bindPort: string;
  targetHost: string;
  targetPort: string;
}

export type ForwardProblem = "host" | "bindPort" | "targetHost" | "targetPort";

export function emptyDraft(hostId = ""): ForwardDraft {
  return { name: "", hostId, bindPort: "", targetHost: "localhost", targetPort: "" };
}

export function draftOf(tunnel: Tunnel): ForwardDraft {
  return {
    name: tunnel.name,
    hostId: tunnel.hostId,
    bindPort: String(tunnel.bindPort),
    targetHost: tunnel.targetHost ?? "localhost",
    targetPort: tunnel.targetPort == null ? "" : String(tunnel.targetPort),
  };
}

function port(text: string): number | null {
  if (!/^\d{1,5}$/.test(text.trim())) return null;
  const value = Number(text);
  return value >= 1 && value <= 65535 ? value : null;
}

/** `8080 → db:5432`: what a forward does, in a few characters. Forwards made
 *  on the desktop (synced here) read the same way for their kind. */
export function describe(forward: {
  kind?: Tunnel["kind"];
  bindPort: number;
  targetHost: string | null;
  targetPort: number | null;
}): string {
  if (forward.kind === "dynamic") return `${forward.bindPort} SOCKS`;
  if (forward.kind === "remote") return `${forward.bindPort} ← ${forward.targetHost}:${forward.targetPort}`;
  return `${forward.bindPort} → ${forward.targetHost}:${forward.targetPort}`;
}

/** The first thing wrong with a draft, or the input to save. A forward without
 *  a name is called by what it does. */
export function checkDraft(
  draft: ForwardDraft,
): { ok: true; input: Omit<TunnelInput, "id"> } | { ok: false; problem: ForwardProblem } {
  if (!draft.hostId) return { ok: false, problem: "host" };
  const bindPort = port(draft.bindPort);
  if (bindPort === null) return { ok: false, problem: "bindPort" };
  const targetHost = draft.targetHost.trim();
  if (!targetHost || /\s/.test(targetHost)) return { ok: false, problem: "targetHost" };
  const targetPort = port(draft.targetPort);
  if (targetPort === null) return { ok: false, problem: "targetPort" };
  return {
    ok: true,
    input: {
      name: draft.name.trim() || describe({ bindPort, targetHost, targetPort }),
      hostId: draft.hostId,
      kind: "local",
      bindAddress: null,
      bindPort,
      targetHost,
      targetPort,
    },
  };
}
