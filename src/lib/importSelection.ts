import type {
  HostsImportApplyParams,
  ImportDuplicatePolicy,
  ImportGroupMode,
  ImportHost,
} from "./proto.gen";

/** Where the imported hosts go, as the dialog's select holds it. */
export type GroupChoice = { mode: Exclude<ImportGroupMode, "existing"> } | { mode: "existing"; groupId: string };

const EXISTING = "existing:";

export function encodeChoice(choice: GroupChoice): string {
  return choice.mode === "existing" ? `${EXISTING}${choice.groupId}` : choice.mode;
}

export function decodeChoice(value: string): GroupChoice {
  if (value.startsWith(EXISTING)) return { mode: "existing", groupId: value.slice(EXISTING.length) };
  return { mode: value === "none" ? "none" : "source" };
}

/** A host the backend found unusable (it says why in `problem`) cannot be selected. */
export function importable(host: ImportHost): boolean {
  return !host.problem;
}

/** Everything that can be imported and is not stored yet. Hosts that exist
 *  start unselected: the duplicate policy decides what happens when the user
 *  ticks them. */
export function defaultSelection(hosts: ImportHost[]): Set<number> {
  const selected = new Set<number>();
  hosts.forEach((host, index) => {
    if (importable(host) && !host.exists) selected.add(index);
  });
  return selected;
}

export function toggle(selected: Set<number>, hosts: ImportHost[], index: number): Set<number> {
  const next = new Set(selected);
  if (!importable(hosts[index])) return next;
  if (next.has(index)) next.delete(index);
  else next.add(index);
  return next;
}

/** Selects every importable host, or none. */
export function setAll(hosts: ImportHost[], on: boolean): Set<number> {
  const selected = new Set<number>();
  if (on) hosts.forEach((host, index) => importable(host) && selected.add(index));
  return selected;
}

export type AllState = "all" | "some" | "none";

export function allState(selected: Set<number>, hosts: ImportHost[]): AllState {
  const total = hosts.filter(importable).length;
  const count = [...selected].filter((index) => hosts[index] && importable(hosts[index])).length;
  if (count === 0) return "none";
  return count === total ? "all" : "some";
}

export interface Tally {
  create: number;
  update: number;
  /** Selected hosts that exist and are left alone by the policy. */
  skip: number;
}

/** What importing the selection will do under `policy`. */
export function tally(hosts: ImportHost[], selected: Set<number>, policy: ImportDuplicatePolicy): Tally {
  const result: Tally = { create: 0, update: 0, skip: 0 };
  for (const index of selected) {
    const host = hosts[index];
    if (!host || !importable(host)) continue;
    if (!host.exists || policy === "keep-both") result.create += 1;
    else if (policy === "update") result.update += 1;
    else result.skip += 1;
  }
  return result;
}

/** The number on the Import button: hosts that will be written. */
export function importCount(counts: Tally): number {
  return counts.create + counts.update;
}

/** The `hosts.import_apply` payload: the selected hosts in file order,
 *  without the preview-only fields. */
export function buildApply(
  hosts: ImportHost[],
  selected: Set<number>,
  choice: GroupChoice,
  policy: ImportDuplicatePolicy,
): HostsImportApplyParams {
  const chosen = hosts
    .filter((host, index) => selected.has(index) && importable(host))
    .map(({ exists: _exists, problem: _problem, ...host }) => host);
  const params: HostsImportApplyParams = { hosts: chosen, group_mode: choice.mode, duplicates: policy };
  if (choice.mode === "existing") params.group_id = choice.groupId;
  return params;
}

/** `user@host:port`, leaving out the default port. */
export function describeTarget(host: ImportHost): string {
  const user = host.username ? `${host.username}@` : "";
  if (host.port === 22) return `${user}${host.hostname}`;
  const hostname = host.hostname.includes(":") ? `[${host.hostname}]` : host.hostname;
  return `${user}${hostname}:${host.port}`;
}

export function describeGroup(path: string[] | undefined): string {
  return path && path.length > 0 ? path.join(" / ") : "";
}
