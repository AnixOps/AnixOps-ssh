import { Channel, invoke } from "@tauri-apps/api/core";
import { useEffect, useSyncExternalStore } from "react";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { isTauri } from "./backend";
import type { ContextPiece } from "./ai/context";
import { unavailableReason, type AiConfig } from "./ai/providers";
import type { AiEvent, AiProvider, Review } from "./ai/review";
import { previewAi, previewLogs } from "./previewAi";
import {
  exportName,
  type LogSettings,
  type OpenedLog,
  type SessionLogEntry,
} from "./sessionLogs";

/* Everything here talks to Rust. The API key is read there, right before a
   request, and never comes back: the settings only say whether one is saved. */

/* Whether Ask AI can be used right now, shared by every terminal so its
   button only shows once AI is turned on and has what it needs. Unknown
   (still loading, or the read failed) counts as not ready: a button that
   appears late beats one that opens onto "AI is off". */
let aiReady = false;
let aiReadyLoaded = false;
const aiReadyListeners = new Set<() => void>();

function publishAiConfig(config: AiConfig): AiConfig {
  const ready = unavailableReason(config) === null;
  if (ready !== aiReady) {
    aiReady = ready;
    for (const listener of aiReadyListeners) listener();
  }
  return config;
}

/** True once AI is enabled and, where the provider needs one, has a key. */
export function useAiReady(): boolean {
  useEffect(() => {
    if (aiReadyLoaded) return;
    aiReadyLoaded = true;
    aiConfigGet().catch((error) => {
      aiReadyLoaded = false;
      console.error(error);
    });
  }, []);
  return useSyncExternalStore(
    (listener) => {
      aiReadyListeners.add(listener);
      return () => aiReadyListeners.delete(listener);
    },
    () => aiReady,
  );
}

export async function aiConfigGet(): Promise<AiConfig> {
  return publishAiConfig(isTauri ? await invoke<AiConfig>("ai_config_get") : await previewAi.config());
}

export async function aiConfigSave(config: AiConfig): Promise<AiConfig> {
  if (!isTauri) return publishAiConfig(await previewAi.save(config));
  const { enabled, provider, model, baseUrl } = config;
  return publishAiConfig(await invoke<AiConfig>("ai_config_save", { settings: { enabled, provider, model, baseUrl } }));
}

/** Saves the key of `provider` in the vault; null or empty removes it. */
export async function aiKeySet(provider: AiProvider, key: string | null): Promise<AiConfig> {
  return publishAiConfig(
    isTauri ? await invoke<AiConfig>("ai_key_set", { provider, key }) : await previewAi.setKey(provider, key),
  );
}

/** Scrubs the context and returns the exact request for review. Sends nothing. */
export async function aiPrepare(question: string, pieces: ContextPiece[]): Promise<Review> {
  return isTauri ? invoke<Review>("ai_prepare", { question, pieces }) : previewAi.prepare(question, pieces);
}

/** Sends a reviewed request; the answer streams to `onEvent`. Resolves to the id for `aiCancel`. */
export async function aiSend(reviewId: string, onEvent: (event: AiEvent) => void): Promise<string> {
  if (!isTauri) return previewAi.send(reviewId, onEvent);
  const events = new Channel<AiEvent>();
  events.onmessage = onEvent;
  return invoke<string>("ai_send", { reviewId, events });
}

export async function aiCancel(requestId: string): Promise<void> {
  if (!isTauri) return previewAi.cancel(requestId);
  await invoke("ai_cancel", { requestId });
}

// ---- session logs --------------------------------------------------------

export async function logSettingsGet(): Promise<LogSettings> {
  return isTauri ? invoke<LogSettings>("session_logs_settings_get") : previewLogs.settings();
}

export async function logSettingsSave(recordByDefault: boolean, retentionDays: number): Promise<LogSettings> {
  return isTauri
    ? invoke<LogSettings>("session_logs_settings_save", { recordByDefault, retentionDays })
    : previewLogs.saveSettings(recordByDefault, retentionDays);
}

/** One host's choice: true, false, or null to follow the default. */
export async function logHostSet(hostId: string, record: boolean | null): Promise<LogSettings> {
  return isTauri
    ? invoke<LogSettings>("session_log_host_set", { hostId, record })
    : previewLogs.setHost(hostId, record);
}

export async function logList(): Promise<SessionLogEntry[]> {
  return isTauri ? invoke<SessionLogEntry[]>("session_logs_list") : previewLogs.list();
}

/** Decrypts a log. Rejects when it was modified or the vault is locked. */
export async function logOpen(id: string): Promise<OpenedLog> {
  return isTauri ? invoke<OpenedLog>("session_log_open", { id }) : previewLogs.open(id);
}

/** Asks where to save, then writes the log as unencrypted plain text. False if cancelled.
 *  The caller has already asked the user to confirm. */
export async function logExport(entry: SessionLogEntry): Promise<boolean> {
  if (!isTauri) return previewLogs.exported(entry.id);
  const path = await saveDialog({
    defaultPath: exportName(entry),
    filters: [{ name: "Text", extensions: ["txt"] }],
  });
  if (!path) return false;
  await invoke("session_log_export", { id: entry.id, path });
  return true;
}

export async function logDelete(id: string): Promise<void> {
  if (!isTauri) return previewLogs.remove(id);
  await invoke("session_log_delete", { id });
}
