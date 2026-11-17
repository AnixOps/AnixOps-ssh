import { ArrowLeft, LoaderCircle, RotateCw, Send, ShieldCheck, Sparkles, Square, X } from "lucide-react";
import { useCallback, useEffect, useReducer, useRef, useState, type ReactNode } from "react";
import { AiMarkdown } from "@/components/app/AiMarkdown";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { aiCancel, aiConfigGet, aiPrepare, aiSend } from "@/lib/aiBackend";
import { isVaultLocked } from "@/lib/backend";
import {
  availability,
  canReview,
  initialChoice,
  LINE_COUNTS,
  pieces,
  setLineCount,
  toggle,
  type ContextChoice,
  type ContextSource,
} from "@/lib/ai/context";
import { unavailableReason, type AiConfig } from "@/lib/ai/providers";
import { initialState, isBusy, redactionSummary, reduce, sendable, type Review } from "@/lib/ai/review";
import { t } from "@/lib/i18n";
import { requestVaultUnlock } from "@/lib/vaultUi";
import { cn } from "@/lib/utils";

/** Width of the panel; the terminal makes room for it. */
export const ASK_AI_WIDTH = "22rem";

const REDACTION_LABELS: Record<string, string> = {
  "private-key": t.redactPrivateKey,
  "anthropic-key": t.redactAnthropicKey,
  "api-key": t.redactApiKey,
  "github-token": t.redactGithub,
  "gitlab-token": t.redactGitlab,
  "aws-key": t.redactAws,
  "google-key": t.redactGoogle,
  "slack-token": t.redactSlack,
  jwt: t.redactJwt,
  "auth-header": t.redactAuth,
  "url-password": t.redactUrlPassword,
  "password-flag": t.redactPasswordFlag,
  "secret-value": t.redactSecretValue,
};

const label = (kind: string) => REDACTION_LABELS[kind] ?? kind;

/**
 * The "Ask AI" side panel of one terminal: pick context, review the exact
 * request, send it, read the streamed answer. Nothing leaves the device until
 * Send is pressed in the review step.
 */
export function AskAiPanel({
  source,
  onInsert,
  onClose,
}: {
  source: ContextSource;
  /** Puts a command on the terminal's input line without running it; absent when the session is gone. */
  onInsert?: (command: string) => void;
  onClose: () => void;
}) {
  const [config, setConfig] = useState<AiConfig | null>(null);
  const [state, dispatch] = useReducer(reduce, initialState);
  const [choice, setChoice] = useState<ContextChoice>(() => initialChoice(source));
  const [question, setQuestion] = useState("");
  // The terminal's selection changes without this panel knowing.
  const [, refresh] = useState(0);
  const requestRef = useRef<{ id: string | null; stop: boolean }>({ id: null, stop: false });
  const questionRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    aiConfigGet().then(setConfig).catch(console.error);
  }, []);
  useEffect(() => {
    if (state.phase !== "compose") return;
    const timer = window.setInterval(() => refresh((n) => n + 1), 700);
    return () => window.clearInterval(timer);
  }, [state.phase]);
  useEffect(() => {
    if (config && unavailableReason(config) === null) questionRef.current?.focus();
  }, [config]);

  const available = availability(source);
  const reason = config ? unavailableReason(config) : null;

  const prepare = useCallback(async () => {
    dispatch({ type: "prepare" });
    try {
      dispatch({ type: "prepared", review: await aiPrepare(question, pieces(choice, source)) });
    } catch (error) {
      dispatch({ type: "prepareFailed", message: String(error) });
    }
  }, [choice, question, source]);

  const send = async (review: Review) => {
    requestRef.current = { id: null, stop: false };
    dispatch({ type: "send" });
    try {
      const id = await aiSend(review.reviewId, (event) => {
        if (event.kind === "delta") dispatch({ type: "delta", text: event.text });
        else if (event.kind === "done") dispatch({ type: "finished" });
        else if (event.kind === "error") dispatch({ type: "failed", message: event.message });
      });
      requestRef.current.id = id;
      dispatch({ type: "started", requestId: id });
      // Stop was pressed before the id was known.
      if (requestRef.current.stop) void aiCancel(id);
    } catch (error) {
      if (isVaultLocked(error)) requestVaultUnlock();
      dispatch({ type: "failed", message: String(error) });
    }
  };

  const stop = () => {
    requestRef.current.stop = true;
    dispatch({ type: "cancel" });
    if (requestRef.current.id) void aiCancel(requestRef.current.id);
  };

  // Leaving while an answer streams stops it.
  useEffect(
    () => () => {
      const request = requestRef.current;
      request.stop = true;
      if (request.id) void aiCancel(request.id);
    },
    [],
  );

  const review = sendable(state);
  const reviewShown = state.phase === "review" ? state.review : null;
  const answerState =
    state.phase === "sending" || state.phase === "done" || (state.phase === "failed" && state.review !== null)
      ? state
      : null;
  const shownReview = reviewShown ?? (answerState ? answerState.review : null);

  return (
    <aside
      aria-label={t.askAi}
      className="material-sidebar absolute inset-y-0 right-0 z-dropdown flex flex-col border-l border-separator"
      style={{ width: ASK_AI_WIDTH }}
    >
      <header className="flex h-11 shrink-0 items-center gap-2 px-3">
        <Sparkles className="size-4 text-(--accent)" strokeWidth={1.5} />
        <h2 className="flex-1 truncate text-callout font-medium">
          {state.phase === "review" ? t.askAiReviewTitle : t.askAi}
        </h2>
        <Tooltip label={t.askAiClose}>
          <Button variant="quiet" size="icon-sm" aria-label={t.askAiClose} onClick={onClose}>
            <X />
          </Button>
        </Tooltip>
      </header>

      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 pb-3">
        {config && reason && (
          <p className="rounded-sm bg-fill-1 px-3 py-2 text-callout text-label-2">
            {reason === "off" ? t.askAiOff : t.askAiNoKey}
          </p>
        )}

        {config && !reason && (state.phase === "compose" || state.phase === "preparing" || (state.phase === "failed" && !state.review)) && (
          <>
            <fieldset className="space-y-1.5" disabled={isBusy(state)}>
              <legend className="text-callout text-label-2">{t.askAiContext}</legend>
              <p className="text-callout text-label-3">{t.askAiContextHint}</p>
              <CheckRow
                checked={choice.command && available.command}
                disabled={!available.command}
                label={t.askAiCtxCommand}
                hint={available.command ? null : t.askAiCtxCommandNeeds}
                onChange={() => setChoice((current) => toggle(current, "command"))}
              />
              <CheckRow
                checked={choice.selection && available.selection}
                disabled={!available.selection}
                label={t.askAiCtxSelection}
                hint={available.selection ? null : t.askAiCtxSelectionNone}
                onChange={() => setChoice((current) => toggle(current, "selection"))}
              />
              <CheckRow
                checked={choice.lines}
                label={t.askAiCtxLines}
                onChange={() => setChoice((current) => toggle(current, "lines"))}
                trailing={
                  <select
                    aria-label={t.askAiCtxLines}
                    value={choice.lineCount}
                    disabled={!choice.lines}
                    onChange={(event) => setChoice((current) => setLineCount(current, Number(event.target.value)))}
                    className="h-6 rounded-xs bg-fill-1 px-1.5 text-callout text-label-1 outline-none focus-visible:ring-2 focus-visible:ring-(--accent) disabled:opacity-40"
                  >
                    {LINE_COUNTS.map((count) => (
                      <option key={count} value={count}>
                        {count}
                      </option>
                    ))}
                  </select>
                }
              />
            </fieldset>

            <label className="block space-y-1.5">
              <span className="text-callout text-label-2">{t.askAiQuestion}</span>
              <textarea
                ref={questionRef}
                value={question}
                rows={4}
                disabled={isBusy(state)}
                placeholder={t.askAiQuestionPlaceholder}
                onChange={(event) => setQuestion(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && (event.metaKey || event.ctrlKey) && canReview(question)) {
                    event.preventDefault();
                    void prepare();
                  }
                }}
                className="w-full resize-none rounded-sm bg-fill-1 px-3 py-2 text-callout font-normal text-label-1 outline-none placeholder:text-label-3 focus-visible:ring-2 focus-visible:ring-(--accent)"
              />
            </label>

            {state.phase === "failed" && <ErrorNote message={state.message} />}

            <Button variant="primary" disabled={!canReview(question) || isBusy(state)} onClick={() => void prepare()}>
              {state.phase === "preparing" ? <LoaderCircle className="animate-spin motion-reduce:animate-none" /> : <ShieldCheck />}
              {state.phase === "preparing" ? t.askAiPreparing : t.askAiReview}
            </Button>
          </>
        )}

        {reviewShown && (
          <ReviewView
            review={reviewShown}
            footer={
              <div className="flex gap-2">
                <Button variant="primary" onClick={() => review && void send(review)}>
                  <Send />
                  {t.askAiSend}
                </Button>
                <Button variant="secondary" onClick={() => dispatch({ type: "edit" })}>
                  <ArrowLeft />
                  {t.askAiBack}
                </Button>
              </div>
            }
          />
        )}

        {answerState && (
          <section aria-label={t.askAiAnswer} aria-live="polite" className="space-y-3">
            {answerState.phase === "sending" && answerState.answer === "" && (
              <p className="flex items-center gap-2 text-callout text-label-2">
                <LoaderCircle className="size-4 animate-spin motion-reduce:animate-none" strokeWidth={1.5} />
                {t.askAiWaiting}
              </p>
            )}
            {answerState.answer !== "" && <AiMarkdown text={answerState.answer} onInsert={onInsert} />}
            {answerState.phase === "done" && answerState.cancelled && (
              <p className="text-callout text-label-3">{t.askAiStopped}</p>
            )}
            {answerState.phase === "failed" && <ErrorNote message={answerState.message} />}
            <div className="flex flex-wrap gap-2">
              {answerState.phase === "sending" ? (
                <Button variant="secondary" size="sm" onClick={stop}>
                  <Square />
                  {t.askAiStop}
                </Button>
              ) : (
                <Button variant="secondary" size="sm" onClick={() => dispatch({ type: "edit" })}>
                  <RotateCw />
                  {t.askAiAgain}
                </Button>
              )}
            </div>
            {shownReview && (
              <details className="rounded-sm bg-fill-1 px-3 py-2">
                <summary className="cursor-pointer text-callout text-label-2">{t.askAiSentText}</summary>
                <div className="mt-2">
                  <ReviewView review={shownReview} compact />
                </div>
              </details>
            )}
          </section>
        )}
      </div>
    </aside>
  );
}

function ErrorNote({ message }: { message: string }) {
  return (
    <p role="alert" className="selectable rounded-sm bg-danger-soft px-3 py-2 text-callout text-danger">
      <span className="font-medium">{t.askAiFailed}. </span>
      {message}
    </p>
  );
}

function CheckRow({
  checked,
  disabled = false,
  label: text,
  hint,
  trailing,
  onChange,
}: {
  checked: boolean;
  disabled?: boolean;
  label: string;
  hint?: string | null;
  trailing?: ReactNode;
  onChange: () => void;
}) {
  return (
    <div className="flex items-start gap-2">
      <label className={cn("flex min-w-0 flex-1 items-start gap-2", disabled && "opacity-50")}>
        <input
          type="checkbox"
          checked={checked}
          disabled={disabled}
          onChange={onChange}
          className="mt-0.5 size-4 shrink-0 accent-(--accent)"
        />
        <span className="min-w-0">
          <span className="block text-callout font-normal text-label-1">{text}</span>
          {hint && <span className="block text-callout text-label-3">{hint}</span>}
        </span>
      </label>
      {trailing}
    </div>
  );
}

/** The exact request: where it goes, what was removed, and the full text. */
function ReviewView({ review, footer, compact = false }: { review: Review; footer?: ReactNode; compact?: boolean }) {
  const removed = redactionSummary(review.redactions, label);
  return (
    <div className="space-y-3">
      {!compact && (
        <p className="rounded-sm bg-accent-soft px-3 py-2 text-callout text-label-1">{t.askAiReviewNotice}</p>
      )}
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-callout">
        <dt className="text-label-3">{t.aiProvider}</dt>
        <dd className="text-label-1">{providerName(review.provider)}</dd>
        <dt className="text-label-3">{t.aiModel}</dt>
        <dd className="selectable min-w-0 font-mono break-all text-label-1">{review.model}</dd>
        <dt className="text-label-3">{t.askAiEndpoint}</dt>
        <dd className="selectable min-w-0 font-mono break-all text-label-1">{review.endpoint}</dd>
      </dl>
      <p className={cn("text-callout", removed ? "text-warning" : "text-label-3")}>
        {removed ? `${t.askAiRedacted}: ${removed}` : t.askAiRedactedNone}
      </p>
      {review.truncated > 0 && <p className="text-callout text-label-3">{t.askAiTruncated}</p>}
      <PromptText title={t.askAiSystem} text={review.system} />
      <PromptText title={t.askAiMessage} text={review.user} />
      {footer}
    </div>
  );
}

function PromptText({ title, text }: { title: string; text: string }) {
  return (
    <div className="space-y-1">
      <p className="text-callout text-label-2">{title}</p>
      <pre
        aria-label={title}
        className="selectable max-h-72 overflow-auto rounded-sm bg-fill-1 p-2 font-mono text-callout whitespace-pre-wrap break-words text-label-1"
      >
        {text}
      </pre>
    </div>
  );
}

function providerName(provider: Review["provider"]): string {
  return provider === "anthropic" ? t.aiProviderAnthropic : provider === "openai" ? t.aiProviderOpenai : t.aiProviderOllama;
}
