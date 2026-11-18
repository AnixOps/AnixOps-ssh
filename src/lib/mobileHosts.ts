import type { Group, Host } from "./backend";

/** Pure logic of the phone's host list and host form. */

/** The text a jump host is stored as: `user@host` or `user@host:port`, with an
 *  IPv6 address in brackets. Mirrors `jump_text` in the engine, which reads it
 *  back when a connection is opened. */
export function jumpText(host: Pick<Host, "fields">): string {
  const { hostname, username, port } = host.fields;
  const name = hostname.includes(":") ? `[${hostname}]` : hostname;
  const user = username ? `${username}@` : "";
  return port === 22 ? `${user}${name}` : `${user}${name}:${port}`;
}

/** The saved hosts a host can be reached through: not itself, not a serial
 *  port, not a host that is itself reached through this one (a loop). */
export function jumpOptions(hosts: Host[], editingId: string | null): Host[] {
  const editing = editingId ? hosts.find((host) => host.id === editingId) : undefined;
  const editingText = editing ? jumpText(editing) : null;
  return hosts
    .filter(
      (host) =>
        host.id !== editingId &&
        host.source === "saved" &&
        host.fields.serial == null &&
        host.fields.alias == null &&
        host.fields.hostname !== "" &&
        // A host that already goes through the one being edited would loop back.
        !(editingText !== null && host.fields.proxyJump?.split(",").some((hop) => hop.trim() === editingText)),
    )
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** The saved host a stored jump text names, when it is one of `hosts`. */
export function jumpHostFor(hosts: Host[], text: string | null): Host | null {
  if (!text) return null;
  return hosts.find((host) => host.fields.alias == null && host.fields.hostname !== "" && jumpText(host) === text.trim()) ?? null;
}

/** `prod, web ,#db` becomes `["prod", "web", "db"]`: trimmed, without a leading
 *  `#`, unique ignoring case, empty ones dropped. */
export function parseTags(text: string): string[] {
  const seen = new Set<string>();
  const tags: string[] = [];
  for (const part of text.split(/[,，\n]/)) {
    const tag = part.trim().replace(/^#+/, "").trim();
    if (!tag || seen.has(tag.toLowerCase())) continue;
    seen.add(tag.toLowerCase());
    tags.push(tag);
  }
  return tags;
}

export function tagsText(tags: string[]): string {
  return tags.join(", ");
}

/** The name of a host's group, or null when it has none. */
export function groupName(host: Pick<Host, "groupId">, groups: Group[]): string | null {
  return groups.find((group) => group.id === host.groupId)?.name ?? null;
}

/** What the search field matches: name, address, tags and group, ignoring
 *  case. `#prod` matches the tag only. Every word has to match. */
export function matchesQuery(host: Host, groups: Group[], query: string): boolean {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const group = groupName(host, groups)?.toLowerCase() ?? "";
  const tags = host.tags.map((tag) => tag.toLowerCase());
  return words.every((word) => {
    if (word.startsWith("#")) {
      const wanted = word.slice(1);
      return wanted === "" || tags.some((tag) => tag.includes(wanted));
    }
    return (
      host.name.toLowerCase().includes(word) ||
      host.target.toLowerCase().includes(word) ||
      group.includes(word) ||
      tags.some((tag) => tag.includes(word))
    );
  });
}

/** The hosts as the list shows them: favorites first, then by name. A serial
 *  port cannot be opened on a phone. */
export function listHosts(hosts: Host[], groups: Group[], query: string): Host[] {
  return hosts
    .filter((host) => host.fields.serial == null && matchesQuery(host, groups, query))
    .sort((a, b) => Number(b.favorite) - Number(a.favorite) || a.name.localeCompare(b.name));
}
