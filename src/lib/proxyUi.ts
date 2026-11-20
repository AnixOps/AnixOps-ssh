import type { ProxyFields, ProxyKind } from "./backend";

/** The local proxy of the AnixOps Net client: its mixed listener, SOCKS5 on 127.0.0.1:7890. */
export const ANIXOPS_NET_PROXY: ProxyFields = { kind: "socks5", host: "127.0.0.1", port: 7890, username: null };

/** The usual port of each kind, offered when the user picks a kind. */
export const DEFAULT_PROXY_PORT: Record<ProxyKind, number> = { socks5: 1080, http: 8080 };

/** What the editor shows when the user first picks a kind: nothing typed yet. */
export function newProxy(kind: ProxyKind): ProxyFields {
  return { kind, host: "", port: DEFAULT_PROXY_PORT[kind], username: null };
}

/** Switching the kind keeps the address and moves the port only while it is still the old default. */
export function withKind(proxy: ProxyFields, kind: ProxyKind): ProxyFields {
  const port = proxy.port === DEFAULT_PROXY_PORT[proxy.kind] ? DEFAULT_PROXY_PORT[kind] : proxy.port;
  return { ...proxy, kind, port };
}

/** The proxy as it is saved: trimmed, and a blank user means no login. */
export function cleanProxy(proxy: ProxyFields | null | undefined): ProxyFields | null {
  if (!proxy) return null;
  const username = proxy.username?.trim();
  return { kind: proxy.kind, host: proxy.host.trim(), port: Number(proxy.port), username: username ? username : null };
}

/** Whether the proxy can be saved: an address and a port. */
export function proxyReady(proxy: ProxyFields | null | undefined): boolean {
  if (!proxy) return true;
  const port = Number(proxy.port);
  return proxy.host.trim() !== "" && Number.isInteger(port) && port >= 1 && port <= 65535;
}

/** Whether the proxy is the AnixOps Net preset, to mark the one-click button as applied. */
export function isAnixopsNet(proxy: ProxyFields | null | undefined): boolean {
  return (
    !!proxy &&
    proxy.kind === ANIXOPS_NET_PROXY.kind &&
    proxy.host.trim() === ANIXOPS_NET_PROXY.host &&
    Number(proxy.port) === ANIXOPS_NET_PROXY.port
  );
}

/** `socks5://host:port`, for a tooltip or a line of text; never the login. */
export function proxyLabel(proxy: ProxyFields): string {
  const host = proxy.host.includes(":") ? `[${proxy.host}]` : proxy.host;
  return `${proxy.kind}://${host}:${proxy.port}`;
}
