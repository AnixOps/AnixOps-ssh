import { describe, expect, it, vi } from "vitest";

// i18n reads the language from the page URL; the tests run in node.
vi.stubGlobal("window", { location: { search: "?lang=zh" } });

const { canResume, canVerify, NO_HASH_TOOL, resumeNoteText, verifyView } = await import("./transferResume");

describe("verifyView", () => {
  it("shows a match, with the count for a folder", () => {
    expect(verifyView({ status: "match", files: 1 })).toEqual({ tone: "success", text: "SHA-256 一致" });
    expect(verifyView({ status: "match", files: 12 }).text).toBe("SHA-256 一致（12 个文件）");
  });

  it("notes files left out as already equal and files that could not be checked", () => {
    const view = verifyView({ status: "match", files: 3, skippedVerified: 2, unverified: 1 });
    expect(view.tone).toBe("success");
    expect(view.note).toBe("其中 2 个已存在且内容一致，未重传；另有 1 个文件无法校验");
    expect(verifyView({ status: "match", files: 1 }).note).toBeUndefined();
  });

  it("shows a difference in danger with both hashes", () => {
    const view = verifyView({ status: "mismatch", files: 0, localSha256: "aa", remoteSha256: "bb" });
    expect(view.tone).toBe("danger");
    expect(view.text).toBe("SHA-256 不一致");
    expect(view.hashes).toEqual({ local: "aa", remote: "bb" });
  });

  it("says why a check could not run, and names the missing tool", () => {
    expect(verifyView({ status: "unavailable", files: 0, detail: NO_HASH_TOOL }).text).toBe("无法校验：服务器没有 sha256sum");
    expect(verifyView({ status: "unavailable", files: 0 }).text).toBe("无法校验：服务器没有 sha256sum");
    const other = verifyView({ status: "unavailable", files: 0, detail: "permission denied" });
    expect(other).toEqual({ tone: "muted", text: "无法校验：permission denied" });
  });
});

describe("resumeNoteText", () => {
  it("names where it went on, or why it began again", () => {
    expect(resumeNoteText("resumed", 2048)).toBe("已从 2.0 KB 继续");
    expect(resumeNoteText("source_changed", null)).toBe("源文件已变化，已从头开始");
    expect(resumeNoteText("part_missing", undefined)).toBe("未找到部分文件，已从头开始");
  });
});

describe("canResume and canVerify", () => {
  it("resumes failed or cancelled transfers of the built-in engine", () => {
    expect(canResume({ state: "failed", resumable: true })).toBe(true);
    expect(canResume({ state: "cancelled", resumable: true })).toBe(true);
    expect(canResume({ state: "running", resumable: true })).toBe(false);
    expect(canResume({ state: "done", resumable: true })).toBe(false);
    expect(canResume({ state: "failed", resumable: false })).toBe(false);
    expect(canResume({ state: "failed" })).toBe(false);
  });

  it("does not offer to resume a complete file that failed its check", () => {
    expect(canResume({ state: "failed", resumable: true, verify: { status: "mismatch", files: 0 } })).toBe(false);
  });

  it("checks only finished transfers of the built-in engine", () => {
    expect(canVerify({ state: "done", resumable: true })).toBe(true);
    expect(canVerify({ state: "done", resumable: false })).toBe(false);
    expect(canVerify({ state: "failed", resumable: true })).toBe(false);
  });
});
