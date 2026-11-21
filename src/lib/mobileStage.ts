import { invoke } from "@tauri-apps/api/core";
import { isTauri } from "./backend";
import { chunkRanges } from "./mobileFiles";

/** The upload path of the phone's file browser. */

const CHUNK = 512 * 1024;

/** Hands a file the person picked to the engine, which stages it on the phone
 *  (the web view cannot give it a path). Returns the staged path to upload.
 *  `onProgress` gets the bytes staged so far. A failure discards the partial
 *  copy. */
export async function stageFile(file: File, onProgress: (bytes: number) => void, signal?: AbortSignal): Promise<string> {
  if (!isTauri) {
    onProgress(file.size);
    return `/staged/${file.name}`;
  }
  const id = await invoke<string>("stage_begin", { name: file.name });
  try {
    for (const [start, end] of chunkRanges(file.size, CHUNK)) {
      if (signal?.aborted) throw new DOMException("cancelled", "AbortError");
      const bytes = new Uint8Array(await file.slice(start, end).arrayBuffer());
      await invoke("stage_chunk", bytes, { headers: { "x-upload-id": id, "x-offset": String(start) } });
      onProgress(end);
    }
    return await invoke<string>("stage_finish", { id, total: file.size });
  } catch (reason) {
    void invoke("stage_discard", { id }).catch(() => {});
    throw reason;
  }
}

/** Gives up on a staged file that is not going to be uploaded. */
export function discardStaged(path: string): void {
  if (!isTauri) return;
  const id = path.split("/").slice(-2, -1)[0];
  if (id) void invoke("stage_discard", { id }).catch(() => {});
}
