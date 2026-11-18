import { invoke } from "@tauri-apps/api/core";
import { isTauri } from "./backend";

/** Hands a downloaded file to the person. The engine saves it in the app's own
 *  cache; what happens next is the platform's: Android has the share sheet and
 *  the Downloads folder (the `AnixNative` bridge in MainActivity), iOS the
 *  system share sheet, which is also where "Save to Files" lives. */

interface AnixNative {
  shareFile(path: string): string;
  saveToDownloads(path: string): string;
}

function native(): AnixNative | null {
  return (window as unknown as { AnixNative?: AnixNative }).AnixNative ?? null;
}

export type ShareChoice = "share" | "save";

/** What the platform can do with a finished download here. */
export function shareOptions(): ShareChoice[] {
  if (!isTauri) return ["save", "share"];
  if (native()) return ["save", "share"];
  if (typeof navigator.share === "function") return ["share"];
  return [];
}

function lastName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/** Shares or saves the downloaded file at `path`; throws with a message to show. */
export async function deliver(path: string, choice: ShareChoice): Promise<void> {
  if (!isTauri) return;
  const bridge = native();
  if (bridge) {
    const problem = choice === "save" ? bridge.saveToDownloads(path) : bridge.shareFile(path);
    if (problem) throw new Error(problem);
    return;
  }
  // iOS: the web view has no path to give the share sheet, so it gets the bytes.
  const bytes = await invoke<ArrayBuffer>("read_download", { path });
  const file = new File([bytes], lastName(path));
  if (!navigator.canShare?.({ files: [file] })) throw new Error("sharing is not available");
  try {
    await navigator.share({ files: [file] });
  } catch (reason) {
    // Closing the sheet is an answer, not an error.
    if (reason instanceof DOMException && reason.name === "AbortError") return;
    throw reason;
  }
}

/** Removes a download from the phone once it was handed over or dismissed. */
export function forget(path: string): void {
  if (isTauri) void invoke("forget_download", { path }).catch(() => {});
}
