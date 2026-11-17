/* The frontend half of crash reports and usage statistics. Consent lives in
   the settings (crashReports, usageStats); everything else is asked of the
   Rust side, which also scrubs what the page reports. Kept free of DOM and
   Tauri imports at module level so it can be tested directly. */

export interface TelemetryStatus {
  /** The build has a crash report server. Without one nothing is collected. */
  crashAvailable: boolean;
  usageAvailable: boolean;
  pending: number;
}

export interface PendingReports {
  reports: { id: string; text: string }[];
  /** The exact body of the next usage ping, if one is due. */
  usage: string | null;
}

export interface TelemetryChoices {
  crashReports: boolean | null;
  usageStats: boolean;
}

export const UNAVAILABLE: TelemetryStatus = { crashAvailable: false, usageAvailable: false, pending: 0 };

/** The first-run card is shown only when it can matter and nothing was answered. */
export function needsFirstRun(choices: TelemetryChoices, status: TelemetryStatus | null): boolean {
  return Boolean(status?.crashAvailable) && choices.crashReports === null;
}

/** What the Privacy section shows and allows. */
export function privacyView(choices: TelemetryChoices, status: TelemetryStatus | null) {
  const crashAvailable = Boolean(status?.crashAvailable);
  const usageAvailable = Boolean(status?.usageAvailable);
  return {
    crashAvailable,
    usageAvailable,
    /** An unanswered question counts as off. */
    crashOn: crashAvailable && choices.crashReports === true,
    usageOn: usageAvailable && choices.usageStats,
    /** Shown instead of the switches when the build cannot send anything. */
    nothingCollected: !crashAvailable && !usageAvailable,
  };
}

/** The text the "View pending reports" panel shows: exactly what would be sent. */
export function pendingText(pending: PendingReports): string {
  const parts = pending.reports.map((report) => report.text.trimEnd());
  if (pending.usage) parts.push(pending.usage);
  return parts.join("\n\n");
}

export type Reporter = (kind: string, message: string, stack: string) => void | Promise<unknown>;

interface ErrorTarget {
  addEventListener(type: string, listener: (event: Event) => void): void;
}

/** Reports at most this many page errors per page load. */
export const MAX_PAGE_REPORTS = 5;

/** window.onerror and unhandledrejection, both through `report`. The Rust side
    decides whether anything is stored, and scrubs it. */
export function installErrorReporting(target: ErrorTarget, report: Reporter): void {
  let sent = 0;
  const seen = new Set<string>();
  const send = (kind: string, message: string, stack: string) => {
    const key = `${kind}:${message}`;
    if (sent >= MAX_PAGE_REPORTS || seen.has(key)) return;
    seen.add(key);
    sent += 1;
    try {
      const result = report(kind, message, stack);
      if (result instanceof Promise) result.catch(() => {});
    } catch {
      // Reporting must never raise a second error.
    }
  };
  target.addEventListener("error", (raw) => {
    const event = raw as unknown as { message?: string; error?: { message?: string; stack?: string } };
    const error = event.error;
    send("error", String(error?.message ?? event.message ?? "error"), String(error?.stack ?? ""));
  });
  target.addEventListener("unhandledrejection", (raw) => {
    const reason = (raw as unknown as { reason?: { message?: string; stack?: string } | string }).reason;
    if (typeof reason === "object" && reason !== null) {
      send("unhandledrejection", String(reason.message ?? "rejection"), String(reason.stack ?? ""));
    } else {
      send("unhandledrejection", String(reason ?? "rejection"), "");
    }
  });
}
