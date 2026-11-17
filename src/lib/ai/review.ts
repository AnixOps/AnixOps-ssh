/** The ask flow as a small state machine: compose, review, send. The only
 *  way to reach `sending` is the `send` action from `review`, and anything
 *  that changes the question or the context goes back to `compose`, so what
 *  is sent is always exactly what was shown. */

export type AiProvider = "anthropic" | "openai" | "ollama";

export interface Redaction {
  kind: string;
  count: number;
}

/** The exact request, as the backend prepared it. */
export interface Review {
  reviewId: string;
  provider: AiProvider;
  model: string;
  endpoint: string;
  system: string;
  user: string;
  redactions: Redaction[];
  truncated: number;
}

/** What the backend streams while a request runs. */
export type AiEvent =
  | { kind: "delta"; text: string }
  | { kind: "done" }
  | { kind: "error"; message: string }
  | { kind: "cancelled" };

export type AskState =
  | { phase: "compose" }
  | { phase: "preparing" }
  | { phase: "review"; review: Review }
  | { phase: "sending"; review: Review; answer: string; requestId: string | null }
  | { phase: "done"; review: Review; answer: string; cancelled: boolean }
  | { phase: "failed"; review: Review | null; answer: string; message: string };

export type AskAction =
  | { type: "prepare" }
  | { type: "prepared"; review: Review }
  | { type: "prepareFailed"; message: string }
  /** The question or the context changed, or the user went back. */
  | { type: "edit" }
  | { type: "send" }
  | { type: "started"; requestId: string }
  | { type: "delta"; text: string }
  | { type: "finished" }
  | { type: "cancel" }
  | { type: "failed"; message: string };

export const initialState: AskState = { phase: "compose" };

export function reduce(state: AskState, action: AskAction): AskState {
  switch (action.type) {
    case "prepare":
      return state.phase === "compose" || state.phase === "failed" || state.phase === "done"
        ? { phase: "preparing" }
        : state;
    case "prepared":
      return state.phase === "preparing" ? { phase: "review", review: action.review } : state;
    case "prepareFailed":
      return state.phase === "preparing"
        ? { phase: "failed", review: null, answer: "", message: action.message }
        : state;
    case "edit":
      // Going back discards the review: it has to be prepared again.
      return state.phase === "sending" || state.phase === "preparing" ? state : { phase: "compose" };
    case "send":
      return state.phase === "review"
        ? { phase: "sending", review: state.review, answer: "", requestId: null }
        : state;
    case "started":
      return state.phase === "sending" ? { ...state, requestId: action.requestId } : state;
    case "delta":
      return state.phase === "sending" ? { ...state, answer: state.answer + action.text } : state;
    case "finished":
      return state.phase === "sending"
        ? { phase: "done", review: state.review, answer: state.answer, cancelled: false }
        : state;
    case "cancel":
      return state.phase === "sending"
        ? { phase: "done", review: state.review, answer: state.answer, cancelled: true }
        : state;
    case "failed":
      if (state.phase === "sending") {
        return { phase: "failed", review: state.review, answer: state.answer, message: action.message };
      }
      // A send that failed before it started (the vault is locked, for example).
      return state.phase === "review"
        ? { phase: "failed", review: state.review, answer: "", message: action.message }
        : state;
  }
}

/** The review that may be sent now: only one the user is looking at. */
export function sendable(state: AskState): Review | null {
  return state.phase === "review" ? state.review : null;
}

export function isBusy(state: AskState): boolean {
  return state.phase === "preparing" || state.phase === "sending";
}

/** A short line for the redaction summary, `null` when nothing was removed. */
export function redactionSummary(
  redactions: Redaction[],
  label: (kind: string) => string,
): string | null {
  if (redactions.length === 0) return null;
  return redactions.map((r) => `${label(r.kind)} × ${r.count}`).join(", ");
}
