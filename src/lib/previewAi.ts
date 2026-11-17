import type { ContextPiece } from "./ai/context";
import { DEFAULT_MODEL, needsKey, type AiConfig } from "./ai/providers";
import type { AiEvent, AiProvider, Review } from "./ai/review";
import type { LogSettings, OpenedLog, SessionLogEntry } from "./sessionLogs";

/* In-memory stand-ins for the AI and session log backend in browser
   previews and CI screenshots. `?ai=on` starts with Ask AI enabled and a key
   saved. Nothing here makes a network request. */

const on = new URLSearchParams(window.location.search).get("ai") === "on";
let config: AiConfig = {
  enabled: on,
  provider: "anthropic",
  model: DEFAULT_MODEL.anthropic,
  baseUrl: null,
  hasKey: on,
};
const keys = new Set<AiProvider>(on ? ["anthropic"] : []);
const reviews = new Map<string, Review>();
const timers = new Map<string, number>();

function withKey(next: AiConfig): AiConfig {
  return { ...next, hasKey: keys.has(next.provider) };
}

const ANSWER =
  "The build stopped because `main.c` includes a header that is not installed.\n\nInstall the development package, then run `make` again:\n\n```sh\nsudo apt install libssl-dev\n```\n";

export const previewAi = {
  config: async (): Promise<AiConfig> => ({ ...config }),
  save: async (next: AiConfig): Promise<AiConfig> => {
    config = withKey({ ...next });
    return { ...config };
  },
  setKey: async (provider: AiProvider, key: string | null): Promise<AiConfig> => {
    if (key && key.trim()) keys.add(provider);
    else keys.delete(provider);
    config = withKey(config);
    return { ...config };
  },
  prepare: async (question: string, pieces: ContextPiece[]): Promise<Review> => {
    if (!config.enabled) throw new Error("Ask AI is off. Turn it on in Settings, AI.");
    if (needsKey(config) && !config.hasKey) throw new Error("Add an API key in Settings, AI, first.");
    let redactions = 0;
    const scrub = (text: string) =>
      text.replace(/\b([A-Za-z0-9_]*(?:password|secret|token|api_?key)[A-Za-z0-9_]*)=\S+/gi, (_, name: string) => {
        redactions++;
        return `${name}=[REDACTED:secret-value]`;
      });
    const context = pieces.map((piece) => `${piece.kind}:\n\`\`\`text\n${scrub(piece.text)}\n\`\`\``).join("\n\n");
    const review: Review = {
      reviewId: Math.random().toString(36).slice(2),
      provider: config.provider,
      model: config.model || DEFAULT_MODEL[config.provider],
      endpoint: config.baseUrl || "https://api.anthropic.com",
      system: "You are a command-line assistant inside an SSH terminal.",
      user: `${context}${context ? "\n\n" : ""}Question: ${scrub(question.trim())}`,
      redactions: redactions > 0 ? [{ kind: "secret-value", count: redactions }] : [],
      truncated: 0,
    };
    reviews.set(review.reviewId, review);
    return review;
  },
  send: async (reviewId: string, onEvent: (event: AiEvent) => void): Promise<string> => {
    if (!reviews.has(reviewId)) throw new Error("This review has expired. Review the request again.");
    reviews.delete(reviewId);
    const id = `preview-${reviewId}`;
    let sent = 0;
    const step = () => {
      if (sent >= ANSWER.length) {
        timers.delete(id);
        onEvent({ kind: "done" });
        return;
      }
      onEvent({ kind: "delta", text: ANSWER.slice(sent, sent + 6) });
      sent += 6;
      timers.set(id, window.setTimeout(step, 40));
    };
    timers.set(id, window.setTimeout(step, 80));
    return id;
  },
  cancel: async (id: string): Promise<void> => {
    window.clearTimeout(timers.get(id));
    timers.delete(id);
  },
};

let logSettings: LogSettings = { recordByDefault: false, retentionDays: 30, hosts: {} };
let entries: SessionLogEntry[] = [
  { id: "prod-web/20260110T090000Z.esl", host: "prod-web", startedMs: Date.UTC(2026, 0, 10, 9, 0, 0), durationMs: 1_380_000, size: 48_213 },
  { id: "staging/20260108T141500Z.esl", host: "staging", startedMs: Date.UTC(2026, 0, 8, 14, 15, 0), durationMs: 95_000, size: 6_144 },
];

const SAMPLE = "\x1b[32muser@prod-web\x1b[0m:~$ systemctl status nginx\r\n● nginx.service - A high performance web server\r\n   Active: \x1b[32mactive (running)\x1b[0m since Sat 2026-01-10 09:00:12 UTC\r\n\x1b[32muser@prod-web\x1b[0m:~$ echo LOGMARKER\r\nLOGMARKER\r\n";

export const previewLogs = {
  settings: async (): Promise<LogSettings> => ({ ...logSettings, hosts: { ...logSettings.hosts } }),
  saveSettings: async (recordByDefault: boolean, retentionDays: number): Promise<LogSettings> => {
    logSettings = { ...logSettings, recordByDefault, retentionDays };
    return previewLogs.settings();
  },
  setHost: async (hostId: string, record: boolean | null): Promise<LogSettings> => {
    const hosts = { ...logSettings.hosts };
    if (record === null) delete hosts[hostId];
    else hosts[hostId] = record;
    logSettings = { ...logSettings, hosts };
    return previewLogs.settings();
  },
  list: async (): Promise<SessionLogEntry[]> => entries.map((entry) => ({ ...entry })),
  open: async (id: string): Promise<OpenedLog> => {
    const entry = entries.find((candidate) => candidate.id === id);
    if (!entry) throw new Error("the session log was not found");
    return {
      text: SAMPLE,
      complete: !id.startsWith("staging"),
      startedMs: entry.startedMs,
      endedMs: entry.startedMs + entry.durationMs,
    };
  },
  exported: async (_id: string): Promise<boolean> => true,
  remove: async (id: string): Promise<void> => {
    entries = entries.filter((entry) => entry.id !== id);
  },
};
