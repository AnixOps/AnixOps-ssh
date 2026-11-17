import { describe, expect, it } from "vitest";
import { analyze, escapeWord, tokenize } from "./tokenizer";

const words = (input: string) => tokenize(input).map((token) => token.text);

describe("tokenize", () => {
  it("splits on whitespace", () => {
    expect(words("ls  -la\t/tmp")).toEqual(["ls", "-la", "/tmp"]);
  });

  it("resolves quotes", () => {
    expect(words(`echo "a b" 'c d' e"f g"h`)).toEqual(["echo", "a b", "c d", "ef gh"]);
  });

  it("keeps backslashes literal in single quotes and escapes in double quotes", () => {
    expect(words(`'a\\b' "a\\"b" "a\\nb"`)).toEqual(["a\\b", 'a"b', "a\\nb"]);
  });

  it("resolves backslash escapes outside quotes", () => {
    expect(words("my\\ file.txt a\\;b")).toEqual(["my file.txt", "a;b"]);
  });

  it("reads $'..' strings", () => {
    expect(words("echo $'a\\tb\\n' $'it\\'s' $'\\x41'")).toEqual(["echo", "a\tb\n", "it's", "A"]);
  });

  it("emits operators and ends words at them", () => {
    const tokens = tokenize("a|b||c&&d;e&f");
    expect(tokens.map((token) => [token.kind, token.text])).toEqual([
      ["word", "a"],
      ["op", "|"],
      ["word", "b"],
      ["op", "||"],
      ["word", "c"],
      ["op", "&&"],
      ["word", "d"],
      ["op", ";"],
      ["word", "e"],
      ["op", "&"],
      ["word", "f"],
    ]);
  });

  it("does not split operators inside quotes or after escapes", () => {
    expect(words(`echo "a && b" 'c | d' e\\|f`)).toEqual(["echo", "a && b", "c | d", "e|f"]);
  });

  it("reads redirections, including a leading file descriptor", () => {
    const tokens = tokenize("ls 2>err.txt >> out < in");
    expect(tokens.filter((token) => token.kind === "redir").map((token) => token.text)).toEqual(["2>", ">>", "<"]);
    expect(words("ls 2>err.txt")).toEqual(["ls", "2>", "err.txt"]);
  });

  it("marks an unterminated quote", () => {
    expect(tokenize(`"unfinished arg`)[0]).toMatchObject({ text: "unfinished arg", quote: "double" });
    expect(tokenize("'abc")[0].quote).toBe("single");
    expect(tokenize("$'abc")[0].quote).toBe("ansi");
  });

  it("treats # at the start of a word as a comment", () => {
    expect(tokenize("ls # not this | that").map((token) => token.kind)).toEqual(["word", "comment"]);
    expect(words("echo a#b")).toEqual(["echo", "a#b"]);
  });

  it("records where each token sits", () => {
    const [first, second] = tokenize("ls  'a b'");
    expect([first.start, first.end, second.start, second.end]).toEqual([0, 2, 4, 9]);
    expect(second.raw).toBe("'a b'");
  });
});

describe("analyze", () => {
  it("finds the word being typed", () => {
    const result = analyze("git commit -m");
    expect(result.words).toEqual(["git", "commit"]);
    expect(result.current.text).toBe("-m");
  });

  it("starts a new word after trailing whitespace", () => {
    const result = analyze("git commit ");
    expect(result.words).toEqual(["git", "commit"]);
    expect(result.current).toMatchObject({ text: "", start: 11 });
  });

  it("restarts the command after an operator", () => {
    expect(analyze("cd /tmp && git sta").words).toEqual(["git"]);
    expect(analyze("cat f | grep ").words).toEqual(["grep"]);
    expect(analyze("a; b || ").words).toEqual([]);
    expect(analyze("cd /tmp && git sta").segmentStart).toBe(11);
  });

  it("skips redirections and says when a path is expected", () => {
    expect(analyze("ls > out.txt -l ").words).toEqual(["ls", "-l"]);
    expect(analyze("ls > ou").afterRedirect).toBe(true);
    expect(analyze("ls > ").afterRedirect).toBe(true);
  });

  it("skips VAR=value prefixes", () => {
    expect(analyze("FOO=1 BAR=2 make all ").words).toEqual(["make", "all"]);
    expect(analyze("FOO=1").assignment).toBe(true);
  });

  it("reports comments", () => {
    expect(analyze("ls # note").comment).toBe(true);
  });

  it("keeps an open quote on the current word", () => {
    expect(analyze(`cat "my fi`).current).toMatchObject({ text: "my fi", quote: "double", start: 4 });
  });
});

describe("escapeWord", () => {
  it("escapes shell metacharacters outside quotes", () => {
    expect(escapeWord("a b&c", "none")).toBe("a\\ b\\&c");
  });

  it("escapes only what the quote needs", () => {
    expect(escapeWord("it's", "single")).toBe("it'\\''s");
    expect(escapeWord('a"b c', "double")).toBe('a\\"b c');
  });
});
