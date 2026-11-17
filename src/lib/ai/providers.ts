import type { AiProvider } from "./review";

/** Mirrors `Provider` in crates/easyssh-ai/src/provider.rs. */
export const DEFAULT_MODEL: Record<AiProvider, string> = {
  anthropic: "claude-sonnet-5-5",
  openai: "gpt-4o-mini",
  ollama: "llama3.1",
};

export const DEFAULT_BASE_URL: Record<AiProvider, string> = {
  anthropic: "https://api.anthropic.com",
  openai: "https://api.openai.com/v1",
  ollama: "http://localhost:11434",
};

export const PROVIDERS: AiProvider[] = ["anthropic", "openai", "ollama"];

/** What the settings form edits; the key is kept apart and never read back. */
export interface AiConfig {
  enabled: boolean;
  provider: AiProvider;
  /** Empty: the provider's default model. */
  model: string;
  baseUrl: string | null;
  hasKey: boolean;
}

/** Whether a request to this configuration needs an API key (same rule as the backend). */
export function needsKey(config: Pick<AiConfig, "provider" | "baseUrl">): boolean {
  if (config.provider === "anthropic") return true;
  if (config.provider === "ollama") return false;
  const base = (config.baseUrl ?? "").trim().replace(/\/+$/, "");
  return base === "" || base === DEFAULT_BASE_URL.openai;
}

/** Why Ask AI cannot be used yet, or `null` when it can. */
export function unavailableReason(config: AiConfig): "off" | "key" | null {
  if (!config.enabled) return "off";
  if (needsKey(config) && !config.hasKey) return "key";
  return null;
}

/** Switching provider: a model that was the old default becomes the new default. */
export function switchProvider(config: AiConfig, provider: AiProvider): AiConfig {
  const wasDefault = config.model.trim() === "" || config.model === DEFAULT_MODEL[config.provider];
  return {
    ...config,
    provider,
    model: wasDefault ? DEFAULT_MODEL[provider] : config.model,
    baseUrl: null,
  };
}
