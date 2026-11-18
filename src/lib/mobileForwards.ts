import type { Tunnel, TunnelInput, TunnelKind } from "./backend";

/** Pure logic of the phone's port-forward form. Three kinds, all on the host's
 *  one shared connection:
 *  - local (`ssh -L`): a port on the phone that reaches a target as the host sees it;
 *  - remote (`ssh -R`): a port on the host that reaches a target as the phone sees it;
 *  - dynamic (`ssh -D`): a SOCKS5 proxy on the phone, no target.
 *  Android and iOS apps cannot listen on a port below 1024, so a port that
 *  listens on the phone (local, dynamic) must be 1024 or more. */

export interface ForwardDraft {
  kind: TunnelKind;
  name: string;
  hostId: string;
  bindPort: string;
  targetHost: string;
  targetPort: string;
}

export type ForwardProblem = "host" | "bindPort" | "privilegedPort" | "targetHost" | "targetPort";

export const FORWARD_KINDS: TunnelKind[] = ["local", "remote", "dynamic"];

/** The lowest port an app may listen on a phone. */
export const FIRST_PHONE_PORT = 1024;

export function emptyDraft(hostId = ""): ForwardDraft {
  return { kind: "local", name: "", hostId, bindPort: "", targetHost: "localhost", targetPort: "" };
}

export function draftOf(tunnel: Tunnel): ForwardDraft {
  return {
    kind: tunnel.kind,
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
  if (draft.kind !== "remote" && bindPort < FIRST_PHONE_PORT) return { ok: false, problem: "privilegedPort" };
  let targetHost: string | null = null;
  let targetPort: number | null = null;
  if (draft.kind !== "dynamic") {
    targetHost = draft.targetHost.trim();
    if (!targetHost || /\s/.test(targetHost)) return { ok: false, problem: "targetHost" };
    targetPort = port(draft.targetPort);
    if (targetPort === null) return { ok: false, problem: "targetPort" };
  }
  return {
    ok: true,
    input: {
      name: draft.name.trim() || describe({ kind: draft.kind, bindPort, targetHost, targetPort }),
      hostId: draft.hostId,
      kind: draft.kind,
      bindAddress: null,
      bindPort,
      targetHost,
      targetPort,
    },
  };
}
