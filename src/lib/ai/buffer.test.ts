import { describe, expect, it } from "vitest";
import { commandOutput, lastLines, readLines, type BufferLike } from "./buffer";
import { DEFAULT_MODEL, needsKey, switchProvider, unavailableReason, type AiConfig } from "./providers";
import {
  choiceValue,
  exportName,
  formatDuration,
  formatSize,
  hostChoice,
  retentionOptions,
  type LogSettings,
} from "../sessionLogs";

/** Rows as xterm stores them: a wrapped row continues the one above it. */
function buffer(rows: (string | { wrapped: string })[]): BufferLike {
  return {
    length: rows.length,
    getLine: (y) => {
      const row = rows[y];
      if (row === undefined) return undefined;
      const wrapped = typeof row !== "string";
      const text = typeof row === "string" ? row : row.wrapped;
      return { isWrapped: wrapped, translateToString: (trim) => (trim ? text.trimEnd() : text) };
    },
  };
}

describe("terminal buffer text", () => {
  const rows = ["$ ls", "a  b", "$ make", "cc main.c", { wrapped: " -o main" }, "error: boom", "$ ", ""];

  it("joins wrapped rows into one line", () => {
    expect(readLines(buffer(rows), 3, 5)).toBe("cc main.c -o main\nerror: boom");
  });

  it("takes the last lines up to the cursor, skipping blank rows at the end", () => {
    const b = buffer(rows);
    expect(lastLines(b, 7, 2)).toBe("error: boom\n$");
    expect(lastLines(b, 7, 3)).toBe("cc main.c -o main\nerror: boom\n$");
    expect(lastLines(b, 7, 100)).toBe("$ ls\na  b\n$ make\ncc main.c -o main\nerror: boom\n$");
    expect(lastLines(buffer(["", ""]), 1, 5)).toBe("");
  });

  it("reads a command's output between its prompt and the next", () => {
    const b = buffer(rows);
    expect(commandOutput(b, 2, 6, 7)).toBe("cc main.c -o main\nerror: boom");
    // The last command runs to the cursor.
    expect(commandOutput(b, 2, null, 5)).toBe("cc main.c -o main\nerror: boom");
    expect(commandOutput(b, 0, 2, 7)).toBe("a  b");
  });
});

describe("provider settings", () => {
  const base: AiConfig = { enabled: true, provider: "anthropic", model: "", baseUrl: null, hasKey: false };

  it("is off, then needs a key, then is ready", () => {
    expect(unavailableReason({ ...base, enabled: false })).toBe("off");
    expect(unavailableReason(base)).toBe("key");
    expect(unavailableReason({ ...base, hasKey: true })).toBe(null);
    expect(unavailableReason({ ...base, provider: "ollama" })).toBe(null);
  });

  it("needs a key for OpenAI itself but not for a server of your own", () => {
    expect(needsKey({ provider: "openai", baseUrl: null })).toBe(true);
    expect(needsKey({ provider: "openai", baseUrl: "https://api.openai.com/v1/" })).toBe(true);
    expect(needsKey({ provider: "openai", baseUrl: "http://127.0.0.1:8000/v1" })).toBe(false);
    expect(needsKey({ provider: "anthropic", baseUrl: null })).toBe(true);
    expect(needsKey({ provider: "ollama", baseUrl: null })).toBe(false);
  });

  it("keeps a custom model when the provider changes, and replaces a default one", () => {
    const anthropic: AiConfig = { ...base, model: DEFAULT_MODEL.anthropic };
    expect(switchProvider(anthropic, "ollama").model).toBe(DEFAULT_MODEL.ollama);
    expect(switchProvider({ ...base, model: "my-model", baseUrl: "http://x" }, "openai")).toMatchObject({
      model: "my-model",
      baseUrl: null,
      provider: "openai",
    });
    expect(DEFAULT_MODEL.anthropic).toBe("claude-sonnet-5-5");
  });
});

describe("session log helpers", () => {
  const settings: LogSettings = { recordByDefault: false, retentionDays: 30, hosts: { a: true, b: false } };

  it("maps a host's setting to a choice and back", () => {
    expect(hostChoice(settings, "a")).toBe("on");
    expect(hostChoice(settings, "b")).toBe("off");
    expect(hostChoice(settings, "c")).toBe("default");
    expect(choiceValue("on")).toBe(true);
    expect(choiceValue("off")).toBe(false);
    expect(choiceValue("default")).toBeNull();
  });

  it("offers the standard retention choices plus a custom one", () => {
    expect(retentionOptions(30)).toEqual([7, 30, 90, 365, 0]);
    expect(retentionOptions(45)).toEqual([7, 30, 45, 90, 365, 0]);
    expect(retentionOptions(0)).toEqual([7, 30, 90, 365, 0]);
  });

  it("formats durations, sizes and file names", () => {
    expect(formatDuration(900)).toBe("1 s");
    expect(formatDuration(45_000)).toBe("45 s");
    expect(formatDuration(5 * 60_000 + 10_000)).toBe("5 min");
    expect(formatDuration(3_600_000)).toBe("1 h");
    expect(formatDuration(3_600_000 + 2 * 60_000)).toBe("1 h 2 min");
    expect(formatSize(512)).toBe("512 B");
    expect(formatSize(2048)).toBe("2.0 KB");
    expect(formatSize(200 * 1024)).toBe("200 KB");
    expect(formatSize(3 * 1024 * 1024)).toBe("3.0 MB");
    expect(exportName({ host: "web 1/prod", startedMs: Date.UTC(2026, 0, 2, 3, 4, 5) })).toBe(
      "web_1_prod-2026-01-02T03-04-05.txt",
    );
  });
});
