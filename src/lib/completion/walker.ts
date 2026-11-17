import { asList, type ArgSpec, type Candidate, type CommandSpec, type OptionSpec, type ParserSpec, type Template } from "./types";

export interface WalkResult {
  /** Candidates written in the spec: subcommands, options and static values. */
  candidates: Candidate[];
  /** Generators to ask for more (paths, history, hosts, command names). */
  templates: Template[];
  /** Kept in front of the completed value, such as `--file=`. */
  valuePrefix: string;
  /** What the candidates are matched against. */
  partial: string;
  /** The words, from the wrapped command's name on, when this one runs another command. */
  wrap?: string[];
  /** The spec node the walk ended in, as a command path such as `docker compose`. */
  path: string[];
}

const DEFAULT_PARSER: Required<ParserSpec> = {
  flagClustering: true,
  optionsAfterDoubleDash: false,
  optionsAfterArgs: true,
};

export const namesOf = (name: string | string[]): string[] => asList(name);

function hasTemplate(arg: ArgSpec | undefined, template: Template): boolean {
  return asList(arg?.template).includes(template);
}

/** The argument that the next positional word fills; the last variadic one repeats. */
function argAt(args: ArgSpec[] | undefined, index: number): ArgSpec | undefined {
  if (!args || args.length === 0) return undefined;
  if (index < args.length) return args[index];
  const last = args[args.length - 1];
  return last.variadic ? last : undefined;
}

function requiredArgs(option: OptionSpec): number {
  const args = option.args ?? [];
  let count = 0;
  for (const arg of args) {
    if (arg.optional) break;
    count += 1;
  }
  return count;
}

function suggestionCandidates(arg: ArgSpec): Candidate[] {
  return (arg.suggestions ?? []).map((suggestion) =>
    typeof suggestion === "string"
      ? { label: suggestion, insert: suggestion, kind: "value" as const }
      : { label: suggestion.name, insert: suggestion.name, kind: "value" as const, description: suggestion.description },
  );
}

/**
 * Reads `words` (the finished words after the command's name) against `root`
 * and says what can be typed for the word `current`.
 */
export function walk(root: CommandSpec, words: string[], current: string): WalkResult {
  let node = root;
  const path: string[] = [];
  let parser: Required<ParserSpec> = { ...DEFAULT_PARSER, ...root.parser };
  let inherited: OptionSpec[] = [];
  let options: OptionSpec[] = root.options ?? [];
  const used = new Set<OptionSpec>();
  let argIndex = 0;
  let optionsEnded = false;
  const state: { pending: { option: OptionSpec; index: number } | null } = { pending: null };

  const visible = () => [...options, ...inherited];
  const lookup = (name: string) => visible().find((option) => namesOf(option.name).includes(name));

  /** A value was attached to the option (`-n5`, `--n=5`); more may still be required. */
  const takeInline = (option: OptionSpec) => {
    if (requiredArgs(option) > 1) state.pending = { option, index: 1 };
  };

  const readOption = (word: string) => {
    const exact = lookup(word);
    if (exact) {
      used.add(exact);
      if (!exact.requiresEquals && requiredArgs(exact) > 0) state.pending = { option: exact, index: 0 };
      return;
    }
    const equals = word.indexOf("=");
    if (equals > 0) {
      const option = lookup(word.slice(0, equals));
      if (option) {
        used.add(option);
        takeInline(option);
      }
      return;
    }
    if (!word.startsWith("--") && parser.flagClustering) {
      for (let j = 1; j < word.length; j += 1) {
        const option = lookup(`-${word[j]}`);
        if (!option) continue;
        used.add(option);
        if ((option.args ?? []).length > 0) {
          if (j + 1 < word.length) takeInline(option);
          else if (!option.requiresEquals && requiredArgs(option) > 0) state.pending = { option, index: 0 };
          break;
        }
      }
    }
  };

  for (let i = 0; i < words.length; i += 1) {
    const word = words[i];
    if (state.pending) {
      const { option, index } = state.pending;
      state.pending = index + 1 < requiredArgs(option) ? { option, index: index + 1 } : null;
      continue;
    }
    if (!optionsEnded && word === "--") {
      if (!parser.optionsAfterDoubleDash) optionsEnded = true;
      continue;
    }
    if (!optionsEnded && word.length > 1 && word.startsWith("-")) {
      readOption(word);
      continue;
    }
    if (argIndex === 0 && node.subcommands) {
      const sub = node.subcommands.find((candidate) => namesOf(candidate.name).includes(word));
      if (sub) {
        path.push(namesOf(sub.name)[0]);
        inherited = [...inherited, ...options.filter((option) => option.persistent)];
        node = sub;
        options = sub.options ?? [];
        parser = { ...parser, ...sub.parser };
        used.clear();
        continue;
      }
    }
    const arg = argAt(node.args, argIndex);
    if (hasTemplate(arg, "command")) return { candidates: [], templates: [], valuePrefix: "", partial: current, wrap: words.slice(i), path };
    argIndex += 1;
    if (!parser.optionsAfterArgs) optionsEnded = true;
  }

  const result: WalkResult = { candidates: [], templates: [], valuePrefix: "", partial: current, path };

  const valueFor = (option: OptionSpec, index: number, prefix: string, partial: string) => {
    const arg = option.args?.[index];
    result.valuePrefix = prefix;
    result.partial = partial;
    if (!arg) return;
    result.candidates.push(...suggestionCandidates(arg));
    result.templates.push(...asList(arg.template));
  };

  const pendingNow = state.pending;
  if (pendingNow) {
    valueFor(pendingNow.option, pendingNow.index, "", current);
    return result;
  }

  if (!optionsEnded && current.startsWith("-")) {
    const equals = current.indexOf("=");
    if (equals > 0) {
      const option = lookup(current.slice(0, equals));
      if (option && (option.args ?? []).length > 0) {
        valueFor(option, 0, current.slice(0, equals + 1), current.slice(equals + 1));
      }
      return result;
    }
    const usedNames = new Set([...used].flatMap((option) => namesOf(option.name)));
    for (const option of visible()) {
      const names = namesOf(option.name);
      if (!option.repeatable && used.has(option)) continue;
      const exclusive = option.exclusiveWith ?? [];
      if (exclusive.some((name) => usedNames.has(name))) continue;
      if ([...used].some((other) => (other.exclusiveWith ?? []).some((name) => names.includes(name)))) continue;
      for (const name of names) {
        result.candidates.push({
          label: name,
          insert: option.requiresEquals ? `${name}=` : name,
          kind: "option",
          description: option.description,
          keepOpen: option.requiresEquals || undefined,
        });
      }
    }
    if (parser.flagClustering && /^-[^-=]+$/.test(current) && current.length >= 2) {
      const flags = [...current.slice(1)].map((char) => lookup(`-${char}`));
      if (flags.every((flag) => flag && (flag.args ?? []).length === 0)) {
        for (const option of visible()) {
          for (const name of namesOf(option.name)) {
            if (name.length !== 2 || current.includes(name[1])) continue;
            result.candidates.push({ label: `${current}${name[1]}`, insert: `${current}${name[1]}`, kind: "option", description: option.description });
          }
        }
      }
    }
    return result;
  }

  if (argIndex === 0 && node.subcommands) {
    for (const sub of node.subcommands) {
      for (const name of namesOf(sub.name)) {
        result.candidates.push({ label: name, insert: name, kind: "subcommand", description: sub.description });
      }
    }
  }
  const arg = argAt(node.args, argIndex);
  if (arg) {
    result.candidates.push(...suggestionCandidates(arg));
    result.templates.push(...asList(arg.template));
  }
  return result;
}
