import { describe, expect, it } from "vitest";
import { ShellIntegrationLineSource } from "./lineSource";

function typed(source: ShellIntegrationLineSource, text: string) {
  source.feed(text);
}

describe("ShellIntegrationLineSource before the shell reports", () => {
  it("behaves like the keystroke tracker", () => {
    const source = new ShellIntegrationLineSource();
    const lines: string[] = [];
    source.onCommit((line) => lines.push(line));
    typed(source, "ls -l");
    expect(source.line()).toBe("ls -l");
    typed(source, "\r");
    expect(lines).toEqual(["ls -l"]);
    expect(source.isActive()).toBe(false);
    expect(source.cwd()).toBeUndefined();
  });
});

describe("ShellIntegrationLineSource once the shell is ready", () => {
  function ready() {
    const source = new ShellIntegrationLineSource();
    const lines: string[] = [];
    source.onCommit((line) => lines.push(line));
    source.handle({ kind: "ready", shell: "bash" });
    source.handle({ kind: "cwd", path: "/home/me" });
    source.handle({ kind: "promptStart" });
    source.handle({ kind: "promptEnd" });
    return { source, lines };
  }

  it("reports the shell's directory", () => {
    const { source } = ready();
    expect(source.isActive()).toBe(true);
    expect(source.cwd()).toBe("/home/me");
    source.handle({ kind: "cwd", path: "/tmp" });
    expect(source.cwd()).toBe("/tmp");
  });

  it("commits the line the shell ran, not the keystrokes", () => {
    const { source, lines } = ready();
    // The user typed `!!`; the shell expanded it before running.
    typed(source, "!!\r");
    expect(lines).toEqual([]);
    source.handle({ kind: "commandStart", command: "sudo systemctl restart nginx" });
    expect(lines).toEqual(["sudo systemctl restart nginx"]);
  });

  it("records nothing for a line the shell did not remember", () => {
    const { source, lines } = ready();
    typed(source, " secret\r");
    source.handle({ kind: "commandStart", command: "" });
    expect(lines).toEqual([]);
  });

  it("offers no line while a command runs", () => {
    const { source } = ready();
    typed(source, "cat");
    expect(source.line()).toBe("cat");
    source.handle({ kind: "commandStart", command: "cat" });
    expect(source.line()).toBeNull();
    // What the user types now goes to the program, not into a line.
    typed(source, "hunter2");
    source.handle({ kind: "commandEnd", exitCode: 0 });
    source.handle({ kind: "promptStart" });
    expect(source.line()).toBe("");
  });

  it("starts from an empty line at every prompt", () => {
    const { source } = ready();
    typed(source, "half typed");
    source.handle({ kind: "promptStart" });
    expect(source.line()).toBe("");
    typed(source, "ls");
    expect(source.line()).toBe("ls");
  });
});
