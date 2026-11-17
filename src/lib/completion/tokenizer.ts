/** A small POSIX shell tokenizer, enough to find the word being completed. */

export type Quote = "none" | "single" | "double" | "ansi";

export interface Token {
  kind: "word" | "op" | "redir" | "comment";
  /** The word with quotes and escapes resolved. */
  text: string;
  raw: string;
  start: number;
  end: number;
  /** The quote still open at the end of the token ("none" when balanced). */
  quote: Quote;
}

const OPERATOR_CHARS = new Set(["|", "&", ";", "(", ")", "<", ">", "`", "\n"]);
const ANSI_ESCAPES: Record<string, string> = {
  n: "\n",
  t: "\t",
  r: "\r",
  a: "\x07",
  b: "\b",
  e: "\x1b",
  E: "\x1b",
  f: "\f",
  v: "\v",
  "\\": "\\",
  "'": "'",
  '"': '"',
  "?": "?",
};

function isSpace(char: string): boolean {
  return char === " " || char === "\t";
}

function readOperator(input: string, at: number): { kind: "op" | "redir"; length: number } | null {
  const char = input[at];
  const two = input.slice(at, at + 2);
  if (char === "<" || char === ">") {
    if (two === ">>" || two === "<<" || two === ">&" || two === "<&") return { kind: "redir", length: 2 };
    return { kind: "redir", length: 1 };
  }
  if (char === "&" && two === "&>") return { kind: "redir", length: 2 };
  if (two === "||" || two === "&&" || two === "|&" || two === ";;") return { kind: "op", length: 2 };
  if (OPERATOR_CHARS.has(char)) return { kind: "op", length: 1 };
  return null;
}

/** Splits `input` into words, operators, redirections and comments. */
export function tokenize(input: string): Token[] {
  const tokens: Token[] = [];
  let i = 0;
  while (i < input.length) {
    const char = input[i];
    if (isSpace(char)) {
      i += 1;
      continue;
    }
    const operator = readOperator(input, i);
    if (operator) {
      const raw = input.slice(i, i + operator.length);
      tokens.push({ kind: operator.kind, text: raw, raw, start: i, end: i + operator.length, quote: "none" });
      i += operator.length;
      continue;
    }
    if (char === "#") {
      tokens.push({ kind: "comment", text: input.slice(i), raw: input.slice(i), start: i, end: input.length, quote: "none" });
      break;
    }

    const start = i;
    let text = "";
    let quote: Quote = "none";
    let emitted = false;
    while (i < input.length) {
      const c = input[i];
      if (isSpace(c)) break;
      const op = readOperator(input, i);
      if (op) {
        // `2>file`: a number right before a redirection belongs to it.
        if (op.kind === "redir" && /^\d+$/.test(text) && input.slice(start, i) === text) {
          i += op.length;
          tokens.push({ kind: "redir", text: input.slice(start, i), raw: input.slice(start, i), start, end: i, quote: "none" });
          emitted = true;
        }
        break;
      }
      if (c === "\\") {
        if (i + 1 >= input.length) {
          i += 1;
          break;
        }
        if (input[i + 1] === "\n") {
          i += 2;
          continue;
        }
        text += input[i + 1];
        i += 2;
      } else if (c === "'") {
        const close = input.indexOf("'", i + 1);
        if (close < 0) {
          text += input.slice(i + 1);
          quote = "single";
          i = input.length;
        } else {
          text += input.slice(i + 1, close);
          i = close + 1;
        }
      } else if (c === '"') {
        i += 1;
        let closed = false;
        while (i < input.length) {
          const d = input[i];
          if (d === '"') {
            closed = true;
            i += 1;
            break;
          }
          if (d === "\\" && i + 1 < input.length && '$`"\\\n'.includes(input[i + 1])) {
            if (input[i + 1] !== "\n") text += input[i + 1];
            i += 2;
          } else {
            text += d;
            i += 1;
          }
        }
        if (!closed) quote = "double";
      } else if (c === "$" && input[i + 1] === "'") {
        i += 2;
        let closed = false;
        while (i < input.length) {
          const d = input[i];
          if (d === "'") {
            closed = true;
            i += 1;
            break;
          }
          if (d === "\\" && i + 1 < input.length) {
            const next = input[i + 1];
            const hex = /^x([0-9a-fA-F]{1,2})/.exec(input.slice(i + 1));
            if (hex) {
              text += String.fromCharCode(parseInt(hex[1], 16));
              i += 1 + hex[0].length;
            } else {
              text += ANSI_ESCAPES[next] ?? next;
              i += 2;
            }
          } else {
            text += d;
            i += 1;
          }
        }
        if (!closed) quote = "ansi";
      } else {
        text += c;
        i += 1;
      }
    }
    if (emitted) continue;
    tokens.push({ kind: "word", text, raw: input.slice(start, i), start, end: i, quote });
  }
  return tokens;
}

export interface Analysis {
  /** Finished words of the current command, without redirections and `VAR=x` prefixes. */
  words: string[];
  /** The word being completed (empty when the line ends in whitespace). */
  current: { text: string; raw: string; start: number; quote: Quote };
  /** Where the current command (after the last operator) begins. */
  segmentStart: number;
  /** The word being completed is the target of a redirection. */
  afterRedirect: boolean;
  /** The cursor is inside a comment. */
  comment: boolean;
  /** The word being completed is a `VAR=value` prefix before the command. */
  assignment: boolean;
}

const ASSIGNMENT = /^[A-Za-z_][A-Za-z0-9_]*=/;

/** Works out what to complete at the end of `line`. */
export function analyze(line: string): Analysis {
  const tokens = tokenize(line);
  let current: Analysis["current"] = { text: "", raw: "", start: line.length, quote: "none" };
  const last = tokens[tokens.length - 1];
  let previous = tokens;
  let comment = false;
  if (last && last.end === line.length) {
    if (last.kind === "word") {
      current = { text: last.text, raw: last.raw, start: last.start, quote: last.quote };
      previous = tokens.slice(0, -1);
    } else if (last.kind === "comment") {
      comment = true;
      previous = tokens.slice(0, -1);
    }
  }
  let from = 0;
  for (let i = previous.length - 1; i >= 0; i -= 1) {
    if (previous[i].kind === "op") {
      from = i + 1;
      break;
    }
  }
  const segment = previous.slice(from);
  const segmentStart = segment.length > 0 ? segment[0].start : current.start;
  const words: string[] = [];
  let afterRedirect = false;
  let commandStarted = false;
  for (const token of segment) {
    if (token.kind === "redir") {
      afterRedirect = true;
      continue;
    }
    if (token.kind !== "word") continue;
    if (afterRedirect) {
      afterRedirect = false;
      continue;
    }
    if (!commandStarted && ASSIGNMENT.test(token.raw)) continue;
    commandStarted = true;
    words.push(token.text);
  }
  const assignment = !commandStarted && !afterRedirect && ASSIGNMENT.test(current.raw);
  return { words, current, segmentStart, afterRedirect, comment, assignment };
}

/** Quotes `text` so that the shell reads it back as one word, in the quote context of the word being typed. */
export function escapeWord(text: string, quote: Quote): string {
  if (quote === "single" || quote === "ansi") return text.replace(/'/g, "'\\''");
  if (quote === "double") return text.replace(/(["\\$`])/g, "\\$1");
  return text.replace(/([\s\\!"#$&'()*;<>?[\]^`{|}])/g, "\\$1");
}
