import { KeystrokeLineSource, type CursorPosition, type LineSource } from "@/lib/completion/lineSource";
import type { IntegrationEvent } from "./osc";

/**
 * The line source for a terminal whose shell has the integration script.
 *
 * Until the shell reports that it is ready this behaves exactly like the
 * keystroke tracker it wraps. Once it is active:
 *  - committed lines are the ones the shell reports (history expansion done,
 *    nothing for lines the shell chose not to remember), not a guess from
 *    keystrokes;
 *  - the working directory is the shell's own, so relative paths can be
 *    completed;
 *  - while a command runs there is no line to complete, so a program that
 *    reads input (a password, a pager) never gets suggestions.
 *
 * What is typed at the prompt is still followed by keystrokes: no shell
 * reports its edit buffer.
 */
export class ShellIntegrationLineSource implements LineSource {
  private readonly typed: KeystrokeLineSource;
  private active = false;
  private running = false;
  private directory: string | undefined;
  private listeners: ((line: string) => void)[] = [];
  private changeListeners: (() => void)[] = [];

  constructor(probe: (() => CursorPosition | null) | null = null) {
    this.typed = new KeystrokeLineSource(probe);
    // Before the shell reports itself, keystrokes are all there is.
    this.typed.onCommit((line) => {
      if (!this.active) this.commit(line);
    });
  }

  /** True once the shell's script reported that it is loaded. */
  isActive(): boolean {
    return this.active;
  }

  /** Applies what the shell reported through the OSC sequence. */
  handle(event: IntegrationEvent): void {
    switch (event.kind) {
      case "ready":
        this.active = true;
        this.changed();
        break;
      case "promptStart":
        this.running = false;
        this.typed.reset();
        break;
      case "commandStart":
        this.running = true;
        this.typed.reset();
        if (event.command.trim() !== "") this.commit(event.command);
        break;
      case "cwd":
        this.directory = event.path;
        this.changed();
        break;
      case "commandEnd":
      case "promptEnd":
        break;
    }
  }

  /** Called when the shell reports itself or a new working directory. */
  onChange(listener: () => void): void {
    this.changeListeners.push(listener);
  }

  private changed() {
    for (const listener of this.changeListeners) listener();
  }

  private commit(line: string) {
    for (const listener of this.listeners) listener(line);
  }

  onCommit(listener: (line: string) => void): void {
    this.listeners.push(listener);
  }

  cwd(): string | undefined {
    return this.directory;
  }

  feed(data: string): void {
    if (this.active && this.running) return;
    this.typed.feed(data);
  }

  line(): string | null {
    if (this.active && this.running) return null;
    return this.typed.line();
  }

  setAlternateScreen(on: boolean): void {
    this.typed.setAlternateScreen(on);
  }

  verify(cursor: CursorPosition): void {
    this.typed.verify(cursor);
  }

  reset(): void {
    this.typed.reset();
  }
}
