import type { Tunnel, TunnelStatus } from "./backend";
import { t } from "./i18n";

/** "15432 → db.internal:5432", or "SOCKS 1080". */
export function describeTunnel(
  tunnel: Pick<Tunnel, "kind" | "bindAddress" | "bindPort" | "targetHost" | "targetPort">,
): string {
  const bind = tunnel.bindAddress ? `${tunnel.bindAddress}:${tunnel.bindPort}` : String(tunnel.bindPort);
  if (tunnel.kind === "dynamic") return `SOCKS ${bind}`;
  const target = `${tunnel.targetHost ?? ""}:${tunnel.targetPort ?? ""}`;
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
