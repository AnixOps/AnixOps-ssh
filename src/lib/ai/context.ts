/** What the user can hand to the assistant, and how the choice turns into
 *  the pieces that go to the review step. Pure, so it can be tested without
 *  a terminal. */

export type ContextKind = "command" | "selection" | "lines";

/** Same shape as the backend's `ContextPiece`. */
export interface ContextPiece {
  kind: ContextKind;
  text: string;
}

/** The last command that finished or is running, from the shell integration marks. */
export interface LastCommand {
  command: string | null;
  exitCode: number | null;
  output: string;
}

/** Where the text comes from; the terminal view provides it. */
export interface ContextSource {
  selection(): string;
  /** `null` when the host has no shell integration or no command ran yet. */
  lastCommand(): LastCommand | null;
  lastLines(count: number): string;
}

export interface ContextChoice {
  command: boolean;
  selection: boolean;
  lines: boolean;
  lineCount: number;
}

export const LINE_COUNTS = [20, 50, 100, 200] as const;
export const DEFAULT_LINE_COUNT = 50;

export interface Availability {
  command: boolean;
  selection: boolean;
  lines: boolean;
}

export function availability(source: ContextSource): Availability {
  const last = source.lastCommand();
  return {
    command: last !== null && (last.command !== null || last.output.trim() !== ""),
    selection: source.selection().trim() !== "",
    lines: true,
  };
}

/** Nothing is chosen, except the selection when there is one. */
export function initialChoice(source: ContextSource): ContextChoice {
  return {
    command: false,
    selection: availability(source).selection,
    lines: false,
    lineCount: DEFAULT_LINE_COUNT,
  };
}

export function toggle(choice: ContextChoice, key: "command" | "selection" | "lines"): ContextChoice {
  return { ...choice, [key]: !choice[key] };
}

export function setLineCount(choice: ContextChoice, count: number): ContextChoice {
  const allowed = (LINE_COUNTS as readonly number[]).includes(count) ? count : DEFAULT_LINE_COUNT;
  return { ...choice, lineCount: allowed };
}

/** The command and its output as one block of text. */
export function describeCommand(last: LastCommand): string {
  const head = last.command ? `$ ${last.command}\n` : "";
  const tail = last.exitCode === null ? "" : `\n[exit code ${last.exitCode}]`;
  return `${head}${last.output.replace(/\n+$/, "")}${tail}`;
}

/** The pieces for the checked sources, in a fixed order; empty ones are left out. */
export function pieces(choice: ContextChoice, source: ContextSource): ContextPiece[] {
  const out: ContextPiece[] = [];
  if (choice.command) {
    const last = source.lastCommand();
    if (last) out.push({ kind: "command", text: describeCommand(last) });
  }
  if (choice.selection) out.push({ kind: "selection", text: source.selection() });
  if (choice.lines) out.push({ kind: "lines", text: source.lastLines(choice.lineCount) });
  return out.filter((piece) => piece.text.trim() !== "");
}

/** Whether a question can be reviewed: it has words. Context is optional. */
export function canReview(question: string): boolean {
  return question.trim().length > 0;
}
