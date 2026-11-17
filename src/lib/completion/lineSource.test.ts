import { describe, expect, it } from "vitest";
import { displayWidth, expectedCursor, KeystrokeLineSource, type CursorPosition } from "./lineSource";

/** A source without cursor checking, plus the lines it committed. */
function plain() {
  const source = new KeystrokeLineSource();
  const committed: string[] = [];
  source.onCommit((line) => committed.push(line));
  return { source, committed };
}

describe("KeystrokeLineSource", () => {
  it("collects printable characters", () => {
    const { source } = plain();
    source.feed("ls");
    source.feed(" -la");
    expect(source.line()).toBe("ls -la");
  });

  it("handles backspace, including over wide and astral characters", () => {
    const { source } = plain();
    source.feed("abc\x7f\x7f");
    expect(source.line()).toBe("a");
    source.feed("世😀\x7f\x08");
    expect(source.line()).toBe("a");
    source.feed("\x7f\x7f\x7f");
    expect(source.line()).toBe("");
  });

  it("clears the line with Ctrl+U and the last word with Ctrl+W", () => {
    const { source } = plain();
    source.feed("git commit -m");
    source.feed("\x17");
    expect(source.line()).toBe("git commit ");
    source.feed("\x17");
    expect(source.line()).toBe("git ");
    source.feed("\x15");
    expect(source.line()).toBe("");
  });

  it("commits on Enter and starts over", () => {
    const { source, committed } = plain();
    source.feed("echo hi\r");
    expect(committed).toEqual(["echo hi"]);
    expect(source.line()).toBe("");
    source.feed("  \r\r");
    expect(committed).toEqual(["echo hi"]);
  });

  it("drops the line on Ctrl+C without committing", () => {
    const { source, committed } = plain();
    source.feed("rm -rf /\x03");
    expect(source.line()).toBe("");
    expect(committed).toEqual([]);
  });

  it("gives up on arrows and escape sequences until the next Enter", () => {
    const { source, committed } = plain();
    source.feed("ab\x1b[D");
    expect(source.line()).toBeNull();
    source.feed("cd");
    expect(source.line()).toBeNull();
    source.feed("\r");
    expect(committed).toEqual([]);
    expect(source.line()).toBe("");
    source.feed("\x1b[A");
    expect(source.line()).toBeNull();
    source.feed("\x03x");
    expect(source.line()).toBe("x");
  });

  it("gives up on Tab, Alt+key and other control characters", () => {
    for (const key of ["\t", "\x1bb", "\x01", "\x05", "\x0b", "\x1b[1;5C", "\x1bOH"]) {
      const { source } = plain();
      source.feed(`ab${key}`);
      expect(source.line(), JSON.stringify(key)).toBeNull();
    }
  });

  it("takes bracketed paste without newlines, rejects it with newlines", () => {
    const { source } = plain();
    source.feed("echo \x1b[200~hello world\x1b[201~");
    expect(source.line()).toBe("echo hello world");
    const other = plain().source;
    other.feed("\x1b[200~a\nb\x1b[201~");
    expect(other.line()).toBeNull();
  });

  it("is off in the alternate screen and starts clean when it ends", () => {
    const { source, committed } = plain();
    source.feed("vim f\r");
    source.setAlternateScreen(true);
    source.feed("ihello\r");
    expect(source.line()).toBeNull();
    source.setAlternateScreen(false);
    expect(source.line()).toBe("");
    expect(committed).toEqual(["vim f"]);
  });
});

describe("cursor checks", () => {
  /** A terminal whose cursor the test moves by hand. */
  function tracked(cols = 80) {
    const cursor: CursorPosition = { x: 2, y: 10, cols };
    const source = new KeystrokeLineSource(() => ({ ...cursor }));
    const committed: string[] = [];
    source.onCommit((line) => committed.push(line));
    const echo = (text: string) => {
      const next = expectedCursor({ ...cursor }, text);
      cursor.x = next.x;
      cursor.y = next.y;
      source.verify({ ...cursor });
    };
    return { source, committed, echo, cursor };
  }

  it("shows the line only once the echo has put the cursor where it should be", () => {
    const { source, echo } = tracked();
    source.feed("l");
    expect(source.line()).toBeNull();
    echo("l");
    expect(source.line()).toBe("l");
  });

  it("copes with echo arriving after more keystrokes", () => {
    const { source, cursor } = tracked();
    source.feed("a");
    source.feed("b");
    source.feed("\x7f");
    // The echo of "a" arrives first: it matches an earlier state, not an error.
    cursor.x = 3;
    source.verify({ ...cursor });
    expect(source.line()).toBe("a");
    cursor.x = 4;
    source.verify({ ...cursor });
    expect(source.line()).toBeNull();
    cursor.x = 3;
    source.verify({ ...cursor });
    expect(source.line()).toBe("a");
  });

  it("gives up when the cursor is somewhere unexpected, and never records the line", () => {
    const { source, committed, echo, cursor } = tracked();
    source.feed("ab");
    echo("ab");
    cursor.x = 40;
    source.verify({ ...cursor });
    expect(source.line()).toBeNull();
    source.feed("\r");
    expect(committed).toEqual([]);
  });

  it("does not take input that is not echoed (a password) as a line", () => {
    const { source, committed, cursor } = tracked();
    source.feed("hunter2");
    source.verify({ ...cursor });
    expect(source.line()).toBeNull();
    source.feed("\r");
    expect(committed).toEqual([]);
  });

  it("commits a verified line", () => {
    const { source, committed, echo } = tracked();
    source.feed("ls");
    echo("ls");
    source.feed("\r");
    expect(committed).toEqual(["ls"]);
  });

  it("follows the cursor over a wrapped line", () => {
    const { source, echo, cursor } = tracked(10);
    const text = "x".repeat(12);
    source.feed(text);
    echo(text);
    expect([cursor.x, cursor.y]).toEqual([4, 11]);
    expect(source.line()).toBe(text);
  });
});

describe("displayWidth", () => {
  it("counts wide characters twice and combining marks not at all", () => {
    expect(displayWidth("ab")).toBe(2);
    expect(displayWidth("世界")).toBe(4);
    expect(displayWidth("é")).toBe(1);
  });
});
