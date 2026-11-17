/** Types of the open completion spec format (see completion/README.md). */

export type Template = "filepaths" | "folders" | "remote-paths" | "history" | "hosts" | "command";

export type Suggestion = string | { name: string; description?: string };

export interface ArgSpec {
  name?: string;
  description?: string;
  optional?: boolean;
  variadic?: boolean;
  template?: Template | Template[];
  suggestions?: Suggestion[];
}

export interface OptionSpec {
  name: string | string[];
  description: string;
  args?: ArgSpec[];
  repeatable?: boolean;
  requiresEquals?: boolean;
  exclusiveWith?: string[];
  persistent?: boolean;
}

export interface ParserSpec {
  flagClustering?: boolean;
  optionsAfterDoubleDash?: boolean;
  optionsAfterArgs?: boolean;
}

export interface CommandSpec {
  name: string | string[];
  description: string;
  parser?: ParserSpec;
  options?: OptionSpec[];
  subcommands?: CommandSpec[];
  args?: ArgSpec[];
}

export type CandidateKind =
  | "command"
  | "subcommand"
  | "option"
  | "value"
  | "file"
  | "folder"
  | "history"
  | "host";

export interface Candidate {
  /** What the list shows. */
  label: string;
  /** The raw text that replaces the word being completed. */
  insert: string;
  kind: CandidateKind;
  description?: string;
  /** Do not append a space after inserting (directories, `--opt=`). */
  keepOpen?: boolean;
  /** Index in the line where `insert` starts replacing; defaults to the current word. */
  replaceFrom?: number;
}

/** What the engine knows about the line being edited. */
export interface LineContext {
  line: string;
  /** Working directory of the shell, when shell integration reports it. */
  cwd?: string;
  /** The session's host, or null for a local terminal. */
  hostId: string | null;
}

export const asList = <T>(value: T | T[] | undefined): T[] =>
  value === undefined ? [] : Array.isArray(value) ? value : [value];
