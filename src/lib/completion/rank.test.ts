import { describe, expect, it } from "vitest";
import { matchScore, rank } from "./rank";
import type { Candidate } from "./types";

const item = (label: string): Candidate => ({ label, insert: label, kind: "value" });
const labels = (candidates: Candidate[]) => candidates.map((entry) => entry.label);

describe("matchScore", () => {
  it("ranks prefix over case-insensitive prefix over substring over fuzzy", () => {
    const prefix = matchScore("com", "commit") as number;
    const caseless = matchScore("com", "Commit") as number;
    const substring = matchScore("com", "recommit") as number;
    const fuzzy = matchScore("cmt", "commit") as number;
    expect(prefix).toBeGreaterThan(caseless);
    expect(caseless).toBeGreaterThan(substring);
    expect(substring).toBeGreaterThan(fuzzy);
    expect(fuzzy).toBeGreaterThan(0);
  });

  it("matches option names without their dashes", () => {
    expect(matchScore("verb", "--verbose")).not.toBeNull();
    expect(matchScore("-v", "--verbose")).not.toBeNull();
  });

  it("matches anything for an empty partial", () => {
    expect(matchScore("", "x")).toBe(0);
  });

  it("rejects characters out of order", () => {
    expect(matchScore("tc", "commit")).toBeNull();
  });
});

describe("rank", () => {
  it("puts prefix matches first and keeps the given order between equals", () => {
    const result = labels(rank(["compute", "push", "pull", "cherry-pick", "prune"].map(item), "pu"));
    expect(result).toEqual(["push", "pull", "compute", "prune"]);
  });

  it("keeps the given order when nothing is typed", () => {
    expect(labels(rank(["b", "a", "c"].map(item), ""))).toEqual(["b", "a", "c"]);
  });

  it("falls back to fuzzy matches and drops non-matches", () => {
    expect(labels(rank(["checkout", "cherry-pick", "commit"].map(item), "cpk"))).toEqual(["cherry-pick"]);
  });

  it("drops duplicates and honours the limit", () => {
    expect(rank([item("a"), item("a")], "").length).toBe(1);
    expect(rank(["a", "b", "c"].map(item), "", 2).length).toBe(2);
  });
});
