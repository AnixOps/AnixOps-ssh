import type { Tunnel, TunnelStatus } from "./backend";
import { t } from "./i18n";
import { listeningPort } from "./mobileForwards";

/** An address as it is written before a port: `[::1]` for IPv6. */
function hostForPort(host: string): string {
  return host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
}

/** "15432 → db.internal:5432", or "SOCKS 1080". A tunnel saved with port 0
 *  shows the port it got while it runs, and "auto" otherwise. */
export function describeTunnel(
  tunnel: Pick<Tunnel, "kind" | "bindAddress" | "bindPort" | "targetHost" | "targetPort"> &
    Partial<Pick<Tunnel, "status">>,
): string {
  const listening = listeningPort(tunnel);
  const port = listening === 0 ? t.tunnelAnyPort : String(listening);
  const bind = tunnel.bindAddress ? `${hostForPort(tunnel.bindAddress)}:${port}` : port;
  if (tunnel.kind === "dynamic") return `SOCKS ${bind}`;
  const target = `${hostForPort(tunnel.targetHost ?? "")}:${tunnel.targetPort ?? ""}`;
  return tunnel.kind === "local" ? `${bind} → ${target}` : `${t.remotePort} ${bind} → ${target}`;
}

/** Whether the tunnel is up or on its way there (its switch shows on). */
export function isTunnelActive(status: TunnelStatus): boolean {
  return status.state === "running" || status.state === "starting" || status.state === "reconnecting";
}

/** The warning under a running tunnel whose connections fail, or null. */
export function tunnelWarning(status: TunnelStatus): string | null {
  if (status.state !== "running" || !status.errorCount || !status.lastError) return null;
  return t.tunnelChannelErrors.replace("{count}", String(status.errorCount)).replace("{error}", status.lastError);
}
