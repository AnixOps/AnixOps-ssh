import type { Tunnel } from "./backend";
import { t } from "./i18n";
import { listeningPort } from "./mobileForwards";

/** An address as it is written before a port: `[::1]` for IPv6. */
function hostForPort(host: string): string {
  return host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
}

/** Whether an address field names a Unix domain socket: an absolute path.
 *  Saved tunnels keep a socket path in the same fields as a host, with no
 *  port (bind port 0, no target port). */
export function isSocketPath(value: string | null | undefined): boolean {
  return (value ?? "").trim().startsWith("/");
}

/** A socket path the backend accepts: absolute, no colon, no control characters. */
export function validSocketPath(value: string): boolean {
  const path = value.trim();
  return path.length > 1 && path.startsWith("/") && !path.includes(":") && !/[\u0000-\u001f\u007f]/.test(path);
}

/** Whether this computer can listen on a Unix socket (everywhere but Windows). */
export function canListenOnSocket(): boolean {
  return typeof navigator === "undefined" || !/Win/i.test(navigator.platform);
}

/** "15432 → db.internal:5432", "SOCKS 1080", or "8080 → /run/docker.sock". A
 *  tunnel saved with port 0 shows the port it got while it runs, and "auto"
 *  otherwise. */
export function describeTunnel(
  tunnel: Pick<Tunnel, "kind" | "bindAddress" | "bindPort" | "targetHost" | "targetPort"> &
    Partial<Pick<Tunnel, "status">>,
): string {
  const listening = listeningPort(tunnel);
  const port = listening === 0 ? t.tunnelAnyPort : String(listening);
  const bind = isSocketPath(tunnel.bindAddress)
    ? tunnel.bindAddress!.trim()
    : tunnel.bindAddress
      ? `${hostForPort(tunnel.bindAddress)}:${port}`
      : port;
  if (tunnel.kind === "dynamic") return `SOCKS ${bind}`;
  const target = isSocketPath(tunnel.targetHost)
    ? tunnel.targetHost!.trim()
    : `${hostForPort(tunnel.targetHost ?? "")}:${tunnel.targetPort ?? ""}`;
  return tunnel.kind === "local" ? `${bind} → ${target}` : `${t.remotePort} ${bind} → ${target}`;
}
