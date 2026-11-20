import type { ISearchDecorationOptions, ISearchOptions } from "@xterm/addon-search";

export interface FindFlags {
  caseSensitive: boolean;
  wholeWord: boolean;
  regex: boolean;
}

export const defaultFindFlags: FindFlags = { caseSensitive: false, wholeWord: false, regex: false };

export interface FindResults {
  /** -1 when the active match is unknown (past the highlight limit). */
  index: number;
  count: number;
}

/** Options for the search addon: the three toggles plus highlight-all decorations. */
export function searchOptions(flags: FindFlags, decorations: ISearchDecorationOptions, incremental = false): ISearchOptions {
  return { ...flags, incremental, decorations };
}

/** "3/17" for the active match, "17" when it is not known, "" before anything was typed.
 *  `none` is shown when the query matches nothing. */
export function resultLabel(query: string, results: FindResults | null, none: string): string {
  if (!query) return "";
  if (!results || results.count === 0) return none;
  return results.index >= 0 ? `${results.index + 1}/${results.count}` : String(results.count);
}

/** Whether a regex query compiles, so the bar can say so instead of showing "no results". */
export function validRegex(query: string): boolean {
  try {
    new RegExp(query, "u");
    return true;
  } catch {
    return false;
  }
}
