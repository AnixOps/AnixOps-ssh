import { applyCandidate, suggest, type EngineDeps } from "./engine";
import type { HistoryStore } from "./history";
import { KeystrokeLineSource, type CursorPosition, type LineSource } from "./lineSource";
import { handlePopupKey, openPopup, type PopupKey, type PopupState } from "./popup";

export interface ViewState {
  popup: PopupState | null;
  /** Where the cursor is in the viewport, in cells. */
  cursor: { x: number; y: number };
}

export interface ControllerOptions {
  hostId: string | null;
  deps: EngineDeps;
  history: HistoryStore;
  /** The terminal cursor: absolute row for tracking, viewport row for placing the list. */
  cursor(): { x: number; y: number; absoluteY: number; cols: number };
  /** Sends keystrokes to the PTY. */
  write(data: string): void;
  enabled(): boolean;
  source?: LineSource;
}

/** Ties line tracking, suggestions and the list's keyboard handling together. */
export class CompletionController {
  readonly source: LineSource;
  private view: ViewState = { popup: null, cursor: { x: 0, y: 0 } };
  private listeners = new Set<(view: ViewState) => void>();
  private run = 0;
  private dismissedFor: string | null = null;
  private lastLine: string | null = null;
  private disposed = false;

  constructor(private readonly options: ControllerOptions) {
    this.source =
      options.source ??
      new KeystrokeLineSource(() => {
        const cursor = options.cursor();
        return { x: cursor.x, y: cursor.absoluteY, cols: cursor.cols };
      });
    this.source.onCommit((line) => options.history.add(options.hostId ?? "local", line));
  }

  subscribe(listener: (view: ViewState) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  getView(): ViewState {
    return this.view;
  }

  private publish(view: ViewState) {
    this.view = view;
    for (const listener of this.listeners) listener(view);
  }

  private hide() {
    this.run += 1;
    this.lastLine = null;
    if (this.view.popup) this.publish({ ...this.view, popup: null });
  }

  dispose() {
    this.disposed = true;
    this.listeners.clear();
  }

  /** Keystrokes the user sent (typed, pasted or inserted from the list). */
  input(data: string) {
    this.source.feed(data);
  }

  setAlternateScreen(on: boolean) {
    this.source.setAlternateScreen(on);
    if (on) this.hide();
  }

  /** Call after the terminal parsed output (the echo of what was typed). */
  outputParsed() {
    if (this.disposed) return;
    const cursor = this.options.cursor();
    this.source.verify({ x: cursor.x, y: cursor.absoluteY, cols: cursor.cols } satisfies CursorPosition);
    void this.refresh();
  }

  private async refresh() {
    const line = this.source.line();
    if (!this.options.enabled() || line === null || line === this.dismissedFor) {
      if (line !== this.dismissedFor) this.dismissedFor = null;
      this.hide();
      return;
    }
    this.dismissedFor = null;
    if (line === this.lastLine) return;
    this.lastLine = line;
    const run = (this.run += 1);
    const result = await suggest(
      { line, hostId: this.options.hostId, cwd: this.source.cwd?.() },
      this.options.deps,
    ).catch(() => ({ items: [] }));
    if (run !== this.run || this.disposed) return;
    if (result.items.length === 0) {
      if (this.view.popup) this.publish({ ...this.view, popup: null });
      return;
    }
    const cursor = this.options.cursor();
    this.publish({ popup: openPopup(result.items), cursor: { x: cursor.x, y: cursor.y } });
  }

  /** Returns true when the key was used and must not reach the terminal. */
  handleKey(event: PopupKey): boolean {
    const popup = this.view.popup;
    if (!popup || !this.options.enabled()) return false;
    const outcome = handlePopupKey(popup, event);
    if (!outcome.consumed) return false;
    if (outcome.dismiss) {
      this.dismissedFor = this.source.line();
      this.hide();
    } else if (outcome.accept) {
      this.accept(outcome.accept);
    } else if (outcome.state !== popup) {
      this.publish({ ...this.view, popup: outcome.state });
    }
    return true;
  }

  /** Inserts a chosen item (also used for mouse clicks). */
  accept(item: PopupState["items"][number]) {
    const line = this.source.line();
    if (line === null) return;
    const data = applyCandidate(line, item);
    this.hide();
    this.input(data);
    this.options.write(data);
  }

  highlight(index: number | null) {
    const popup = this.view.popup;
    if (popup && popup.index !== index) this.publish({ ...this.view, popup: { ...popup, index } });
  }
}
