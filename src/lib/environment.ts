/* Host environments: which hosts are production, how they are named, and
   how a search finds them. Pure; the badge and the confirmations build on it. */

import type { Host, HostEnvironment } from "./backend";
import { t } from "./i18n";

export const ENVIRONMENTS: readonly HostEnvironment[] = ["production", "staging", "testing", "development"];

export function environmentName(environment: HostEnvironment): string {
  switch (environment) {
    case "production":
      return t.envProduction;
    case "staging":
      return t.envStaging;
    case "testing":
      return t.envTesting;
    case "development":
      return t.envDevelopment;
  }
}

/** Production is the one the app guards; the others are only named. */
export function isProduction(environment: HostEnvironment | null | undefined): boolean {
  return environment === "production";
}

/** The colour family of a marker: danger for production, warning for staging, none for the rest. */
export function environmentTone(environment: HostEnvironment | null | undefined): "danger" | "warning" | null {
  if (environment === "production") return "danger";
  if (environment === "staging") return "warning";
  return null;
}

/** Whether a search query finds a host by its environment: the shown name
 *  ("生产", "Production") or the stored one ("prod", "production"). `query` is lower case. */
export function environmentMatches(environment: HostEnvironment | null | undefined, query: string): boolean {
  if (!environment || !query) return false;
  return environment.startsWith(query) || environmentName(environment).toLowerCase().includes(query);
}

/** A saved host's environment, by id; hosts that are not saved or not marked are left out. */
export function environmentsById(hosts: readonly Pick<Host, "id" | "environment">[]): Map<string, HostEnvironment> {
  const map = new Map<string, HostEnvironment>();
  for (const host of hosts) if (host.environment) map.set(host.id, host.environment);
  return map;
}

/** "a", "a and b", "a, b and c" is hard to localise; a plain list does. */
export function nameList(names: readonly string[]): string {
  return names.join(", ");
}
