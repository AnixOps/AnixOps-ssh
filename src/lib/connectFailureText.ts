import { classifyStage, triedMethods, type FailureStage } from "./connectFailure";
import { t } from "./i18n";

const TITLES: Record<FailureStage, () => string> = {
  dns: () => t.failTitleDns,
  connect: () => t.failTitleConnect,
  hostkey: () => t.failTitleHostKey,
  auth: () => t.failTitleAuth,
  session: () => t.failTitleSession,
  vault: () => t.failTitleVault,
  cancelled: () => t.failTitleCancelled,
  other: () => t.sessionFailed,
};

const HINTS: Record<FailureStage, (() => string) | null> = {
  dns: () => t.failHintDns,
  connect: () => t.failHintConnect,
  hostkey: () => t.failHintHostKey,
  auth: () => t.failHintAuth,
  session: () => t.failHintSession,
  vault: () => t.failHintVault,
  cancelled: () => t.failHintCancelled,
  other: null,
};

export interface FailureText {
  stage: FailureStage;
  title: string;
  /** What it means and what to check; empty for "other". */
  hint: string;
}

/** The localized words for a failed connect. */
export function failureText(message: string): FailureText {
  const stage = classifyStage(message);
  let hint = HINTS[stage]?.() ?? "";
  if (stage === "auth") {
    const tried = triedMethods(message);
    if (tried.length) hint = `${t.failAuthTried.replace("{methods}", tried.join(", "))}. ${hint}`;
  }
  return { stage, title: TITLES[stage](), hint };
}
