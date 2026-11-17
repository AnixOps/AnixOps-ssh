import index from "../../../completion/index.json";
import type { CommandSpec } from "./types";

/** One entry per command name (aliases included), built by `npm run check:specs -- --write`. */
export interface IndexEntry {
  file: string;
  description: string;
}

export const specIndex = index as Record<string, IndexEntry>;

// One chunk per command: a spec is fetched only when its command is typed.
const loaders = import.meta.glob<{ default: CommandSpec }>("../../../completion/specs/*.json");
const loaded = new Map<string, Promise<CommandSpec | null>>();

export function commandNames(): { name: string; description: string }[] {
  return Object.entries(specIndex).map(([name, entry]) => ({ name, description: entry.description }));
}

/** The spec for a command name such as `git` or `/usr/bin/git`; null when none is written. */
export function loadSpec(command: string): Promise<CommandSpec | null> {
  const name = command.slice(command.lastIndexOf("/") + 1);
  const entry = Object.prototype.hasOwnProperty.call(specIndex, name) ? specIndex[name] : undefined;
  if (!entry) return Promise.resolve(null);
  let spec = loaded.get(entry.file);
  if (!spec) {
    const loader = loaders[`../../../completion/specs/${entry.file}.json`];
    spec = loader ? loader().then((module) => module.default, () => null) : Promise.resolve(null);
    loaded.set(entry.file, spec);
  }
  return spec;
}
