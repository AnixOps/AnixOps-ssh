import type { Candidate } from "./types";

/**
 * How well `partial` matches `text`; null when it does not match at all.
 * Prefix matches beat substrings, which beat fuzzy (in order) matches.
 */
export function matchScore(partial: string, text: string): number | null {
  if (partial === "") return 0;
  if (text.startsWith(partial)) return 1000;
  const lower = text.toLowerCase();
  const needle = partial.toLowerCase();
  if (lower.startsWith(needle)) return 900;
  // `--verbose` also answers `verb`: match after the dashes.
  const bare = lower.replace(/^-+/, "");
  const bareNeedle = needle.replace(/^-+/, "");
  if (bareNeedle !== "" && bare.startsWith(bareNeedle)) return 800;
  const at = lower.indexOf(needle);
  if (at >= 0) return 600 - Math.min(at, 100);
  // Fuzzy: every character of the partial appears in order.
  let position = -1;
  let first = -1;
  for (const char of needle) {
    position = lower.indexOf(char, position + 1);
    if (position < 0) return null;
    if (first < 0) first = position;
  }
  const spread = position - first + 1 - needle.length;
  return Math.max(1, 300 - spread * 10 - first);
}

/** Orders candidates by match quality, keeping the spec's own order between equals. */
export function rank(candidates: Candidate[], partial: string, limit = 50): Candidate[] {
  const scored: { candidate: Candidate; score: number; order: number }[] = [];
  const seen = new Set<string>();
  candidates.forEach((candidate, order) => {
    const score = matchScore(partial, candidate.label);
    if (score === null || seen.has(candidate.insert)) return;
    seen.add(candidate.insert);
    scored.push({ candidate, score, order });
  });
  scored.sort((a, b) => b.score - a.score || a.order - b.order);
  return scored.slice(0, limit).map((entry) => entry.candidate);
}
