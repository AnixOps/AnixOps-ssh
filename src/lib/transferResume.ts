/**
 * Wording and decisions around resuming a transfer and checking it with
 * SHA-256. Pure functions, tested without a window.
 */
import type { VerifyResult } from "./backend";
import { t } from "./i18n";
import { formatSize } from "./remotePath";

type Strings = Pick<
  typeof t,
  | "verifyMatch"
  | "verifyMatchMany"
  | "verifyMismatch"
  | "verifyNoTool"
  | "verifyUnavailable"
  | "transferResumedFrom"
  | "transferRestartChanged"
  | "transferRestartMissing"
>;

/** What the server answers when it has neither `sha256sum` nor `shasum`. */
export const NO_HASH_TOOL = "no sha256sum on the server";

export interface VerifyView {
  tone: "success" | "danger" | "muted";
  text: string;
  /** Both hashes of a difference, to read and copy. */
  hashes?: { local: string; remote: string };
}

export function verifyView(result: VerifyResult, strings: Strings = t): VerifyView {
  if (result.status === "match") {
    return {
      tone: "success",
      text: result.files > 1 ? strings.verifyMatchMany.replace("{n}", String(result.files)) : strings.verifyMatch,
    };
  }
  if (result.status === "mismatch") {
    return {
      tone: "danger",
      text: strings.verifyMismatch,
      hashes: { local: result.localSha256 ?? "", remote: result.remoteSha256 ?? "" },
    };
  }
  const detail = result.detail?.trim();
  return {
    tone: "muted",
    text:
      !detail || detail === NO_HASH_TOOL
        ? strings.verifyNoTool
        : strings.verifyUnavailable.replace("{reason}", detail),
  };
}

/** The line under a row that went on from an earlier try, or began again. */
export function resumeNoteText(
  note: "resumed" | "source_changed" | "part_missing",
  resumedFrom: number | null | undefined,
  strings: Strings = t,
): string {
  if (note === "resumed") return strings.transferResumedFrom.replace("{size}", formatSize(resumedFrom ?? 0));
  return note === "source_changed" ? strings.transferRestartChanged : strings.transferRestartMissing;
}

export interface ResumableItem {
  state: "queued" | "running" | "done" | "failed" | "cancelled";
  resumable?: boolean;
  verify?: VerifyResult;
}

/** A failed or cancelled try can be continued unless it was cut short by the
 *  check itself: its file is complete and a second try would skip it. */
export function canResume(item: ResumableItem): boolean {
  return (item.state === "failed" || item.state === "cancelled") && item.resumable === true && item.verify?.status !== "mismatch";
}

/** Only a finished transfer of a file on the built-in engine can be checked afterwards. */
export function canVerify(item: ResumableItem): boolean {
  return item.state === "done" && item.resumable === true;
}
