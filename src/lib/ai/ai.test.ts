import { describe, expect, it } from "vitest";
import {
  availability,
  canReview,
  describeCommand,
  initialChoice,
  LINE_COUNTS,
  pieces,
  setLineCount,
  toggle,
  type ContextSource,
  type LastCommand,
} from "./context";
import { insertableCommand } from "./commands";
import { parseInline, parseMarkdown } from "./markdown";
import {
  initialState,
  isBusy,
  redactionSummary,
  reduce,
  sendable,
  type AskAction,
  type AskState,
  type Review,
} from "./review";

function source(parts: { selection?: string; last?: LastCommand | null; lines?: string } = {}): ContextSource {
  return {
    selection: () => parts.selection ?? "",
    lastCommand: () => parts.last ?? null,
    lastLines: (count) => (parts.lines ?? "").split("\n").slice(-count).join("\n"),
  };
}

const failed: LastCommand = { command: "make", exitCode: 2, output: "error: boom\n\n" };

describe("context selection", () => {
  it("preselects nothing without a selection", () => {
    const choice = initialChoice(source({ last: failed, lines: "a\nb" }));
    expect(choice).toMatchObject({ command: false, selection: false, lines: false });
    expect(pieces(choice, source({ last: failed, lines: "a\nb" }))).toEqual([]);
  });

  it("preselects only the selection when there is one", () => {
    const s = source({ selection: "Permission denied", last: failed, lines: "x" });
    const choice = initialChoice(s);
    expect(choice).toMatchObject({ command: false, selection: true, lines: false });
    expect(pieces(choice, s)).toEqual([{ kind: "selection", text: "Permission denied" }]);
    expect(availability(source({ selection: "   " })).selection).toBe(false);
  });

  it("offers the last command only when the shell integration knows one", () => {
    expect(availability(source()).command).toBe(false);
    expect(availability(source({ last: failed })).command).toBe(true);
    expect(availability(source({ last: { command: null, exitCode: null, output: "  " } })).command).toBe(false);
  });

  it("builds the pieces in a fixed order from the checked sources", () => {
    const s = source({ selection: "sel", last: failed, lines: "l1\nl2\nl3" });
    let choice = initialChoice(s);
    choice = toggle(toggle(choice, "lines"), "command");
    choice = setLineCount(choice, 20);
    expect(pieces(choice, s).map((p) => p.kind)).toEqual(["command", "selection", "lines"]);
    expect(pieces(toggle(choice, "selection"), s).map((p) => p.kind)).toEqual(["command", "lines"]);
  });

  it("describes the command with its output and exit code", () => {
    expect(describeCommand(failed)).toBe("$ make\nerror: boom\n[exit code 2]");
    expect(describeCommand({ command: null, exitCode: null, output: "text\n" })).toBe("text");
  });

  it("limits the line count to the offered values", () => {
    const base = initialChoice(source());
    expect(setLineCount(base, 100).lineCount).toBe(100);
    expect(setLineCount(base, 12345).lineCount).toBe(50);
    expect(LINE_COUNTS).toContain(50);
  });

  it("leaves empty pieces out and needs a question to review", () => {
    const s = source({ lines: "   \n  " });
    expect(pieces({ command: false, selection: false, lines: true, lineCount: 20 }, s)).toEqual([]);
    expect(canReview("  ")).toBe(false);
    expect(canReview("why?")).toBe(true);
  });
});

const review: Review = {
  reviewId: "r1",
  provider: "anthropic",
  model: "claude-sonnet-5-5",
  endpoint: "https://api.anthropic.com",
  system: "sys",
  user: "usr",
  redactions: [{ kind: "secret-value", count: 2 }],
  truncated: 0,
};

function run(actions: AskAction[], from: AskState = initialState): AskState {
  return actions.reduce(reduce, from);
}

describe("review step", () => {
  it("only sends from the review", () => {
    expect(run([{ type: "send" }]).phase).toBe("compose");
    expect(run([{ type: "prepare" }, { type: "send" }]).phase).toBe("preparing");
    const reviewing = run([{ type: "prepare" }, { type: "prepared", review }]);
    expect(reviewing.phase).toBe("review");
    expect(sendable(reviewing)).toEqual(review);
    expect(sendable(initialState)).toBeNull();
    const sending = reduce(reviewing, { type: "send" });
    expect(sending).toMatchObject({ phase: "sending", answer: "" });
    expect(sendable(sending)).toBeNull();
    // A second Send does not start a second request.
    expect(reduce(sending, { type: "send" })).toBe(sending);
  });

  it("goes back to composing when anything is edited, dropping the review", () => {
    const reviewing = run([{ type: "prepare" }, { type: "prepared", review }]);
    const edited = reduce(reviewing, { type: "edit" });
    expect(edited).toEqual({ phase: "compose" });
    expect(sendable(edited)).toBeNull();
    expect(reduce(edited, { type: "send" })).toBe(edited);
  });

  it("collects streamed text and ends done, cancelled or failed", () => {
    const sending = run([
      { type: "prepare" },
      { type: "prepared", review },
      { type: "send" },
      { type: "started", requestId: "q1" },
      { type: "delta", text: "Run " },
      { type: "delta", text: "`ls`" },
    ]);
    expect(sending).toMatchObject({ phase: "sending", answer: "Run `ls`", requestId: "q1" });
    expect(isBusy(sending)).toBe(true);
    expect(reduce(sending, { type: "finished" })).toMatchObject({ phase: "done", answer: "Run `ls`", cancelled: false });
    expect(reduce(sending, { type: "cancel" })).toMatchObject({ phase: "done", answer: "Run `ls`", cancelled: true });
    expect(reduce(sending, { type: "failed", message: "boom" })).toMatchObject({
      phase: "failed",
      answer: "Run `ls`",
      message: "boom",
    });
  });

  it("ignores late events once the answer is over", () => {
    const done = run([
      { type: "prepare" },
      { type: "prepared", review },
      { type: "send" },
      { type: "cancel" },
    ]);
    expect(done).toMatchObject({ phase: "done", cancelled: true });
    expect(reduce(done, { type: "delta", text: "late" })).toBe(done);
    expect(reduce(done, { type: "failed", message: "late" })).toBe(done);
  });

  it("keeps the review when sending fails before it starts, so it can be sent again after unlocking", () => {
    const reviewing = run([{ type: "prepare" }, { type: "prepared", review }]);
    const failure = reduce(reviewing, { type: "failed", message: "the vault is locked" });
    expect(failure).toMatchObject({ phase: "failed", review });
    // From a failure the user edits and prepares again; nothing is sent as is.
    expect(sendable(failure)).toBeNull();
    expect(reduce(reduce(failure, { type: "edit" }), { type: "prepare" }).phase).toBe("preparing");
  });

  it("fails cleanly when the request cannot be prepared", () => {
    const state = run([{ type: "prepare" }, { type: "prepareFailed", message: "Add an API key" }]);
    expect(state).toMatchObject({ phase: "failed", review: null, message: "Add an API key" });
    expect(reduce(state, { type: "prepare" }).phase).toBe("preparing");
  });

  it("does not leave the busy phases through edit", () => {
    const preparing = reduce(initialState, { type: "prepare" });
    expect(reduce(preparing, { type: "edit" })).toBe(preparing);
    const sending = run([{ type: "prepare" }, { type: "prepared", review }, { type: "send" }]);
    expect(reduce(sending, { type: "edit" })).toBe(sending);
  });

  it("summarises what was redacted", () => {
    const label = (kind: string) => kind.toUpperCase();
    expect(redactionSummary([], label)).toBeNull();
    expect(
      redactionSummary(
        [
          { kind: "a", count: 2 },
          { kind: "b", count: 1 },
        ],
        label,
      ),
    ).toBe("A × 2, B × 1");
  });
});

describe("insertable commands", () => {
  it("accepts one plain line and strips a prompt", () => {
    expect(insertableCommand("ls -la\n")).toBe("ls -la");
    expect(insertableCommand("$ df -h")).toBe("df -h");
    expect(insertableCommand("  sudo systemctl restart nginx  \n\n")).toBe("sudo systemctl restart nginx");
  });

  it("refuses several lines, so a newline can never run part of it", () => {
    expect(insertableCommand("cd /tmp\nrm -rf x")).toBeNull();
    expect(insertableCommand("echo a\r\necho b")).toBeNull();
  });

  it("refuses control and invisible characters", () => {
    expect(insertableCommand("echo hi\x03")).toBeNull();
    expect(insertableCommand("echo \x1b[2Jhi")).toBeNull();
    expect(insertableCommand("echo hi‮")).toBeNull();
    expect(insertableCommand("echo​hi")).toBeNull();
    expect(insertableCommand("echo hi\x7f")).toBeNull();
    expect(insertableCommand("echo\thi")).toBeNull();
  });

  it("refuses empty and huge commands", () => {
    expect(insertableCommand("")).toBeNull();
    expect(insertableCommand("  \n ")).toBeNull();
    expect(insertableCommand("$ ")).toBeNull();
    expect(insertableCommand("x".repeat(2001))).toBeNull();
  });
});

describe("markdown", () => {
  it("reads blocks", () => {
    const blocks = parseMarkdown(
      "# Title\n\nSome text\nover two lines.\n\n- one\n- two\n\n1. first\n2. second\n\n> note\n\n```sh\nls -la\n```\nAfter.",
    );
    expect(blocks.map((b) => b.t)).toEqual(["h", "p", "ul", "ol", "quote", "code", "p"]);
    expect(blocks[1]).toEqual({ t: "p", inline: [{ t: "text", v: "Some text over two lines." }] });
    expect(blocks[5]).toEqual({ t: "code", lang: "sh", code: "ls -la", open: false });
  });

  it("tolerates a code block that is still arriving", () => {
    const blocks = parseMarkdown("Try:\n\n```bash\nsudo systemctl status");
    expect(blocks[1]).toEqual({ t: "code", lang: "bash", code: "sudo systemctl status", open: true });
  });

  it("only closes a fence with a fence at least as long", () => {
    const blocks = parseMarkdown("````text\na\n```\nb\n````");
    expect(blocks).toEqual([{ t: "code", lang: "text", code: "a\n```\nb", open: false }]);
  });

  it("reads inline code, bold, italic and links, and never produces markup", () => {
    expect(parseInline("run `ls` **now** or *later*, see [docs](https://x.test/a)")).toEqual([
      { t: "text", v: "run " },
      { t: "code", v: "ls" },
      { t: "text", v: " " },
      { t: "strong", v: [{ t: "text", v: "now" }] },
      { t: "text", v: " or " },
      { t: "em", v: [{ t: "text", v: "later" }] },
      { t: "text", v: ", see " },
      { t: "link", text: "docs", href: "https://x.test/a" },
    ]);
    // Tags in the answer stay text.
    expect(parseInline("<script>alert(1)</script>")).toEqual([{ t: "text", v: "<script>alert(1)</script>" }]);
    expect(parseInline("my_snake_case_name and 2 * 3 * 4")).toEqual([
      { t: "text", v: "my_snake_case_name and 2 * 3 * 4" },
    ]);
    expect(parseInline("unclosed `code and **bold")).toEqual([{ t: "text", v: "unclosed `code and **bold" }]);
  });
});
