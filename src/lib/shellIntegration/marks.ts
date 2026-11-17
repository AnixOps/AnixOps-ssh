/** What the tracker needs from an xterm marker, so it can be tested without one. */
export interface MarkerLike {
  /** Absolute buffer line; -1 once the line left the scrollback. */
  readonly line: number;
  readonly isDisposed: boolean;
  dispose(): void;
}

export type MarkStatus = "prompt" | "running" | "succeeded" | "failed";

export interface PromptMark<M extends MarkerLike = MarkerLike> {
  readonly marker: M;
  /** The command that was run from this prompt, once there is one. */
  command: string | null;
  exitCode: number | null;
  status: MarkStatus;
}

export interface MarkEvents<M extends MarkerLike = MarkerLike> {
  added(mark: PromptMark<M>): void;
  changed(mark: PromptMark<M>): void;
  removed(mark: PromptMark<M>): void;
}

/**
 * One mark per prompt, with the status of the command run from it. A prompt
 * that was repainted (fish does this) or left empty with Enter is replaced
 * by the next one rather than kept.
 */
export class PromptMarks<M extends MarkerLike = MarkerLike> {
  private marks: PromptMark<M>[] = [];

  constructor(private readonly events: MarkEvents<M>) {}

  all(): readonly PromptMark<M>[] {
    return this.marks;
  }

  promptStart(marker: M): PromptMark<M> {
    const last = this.marks[this.marks.length - 1];
    if (last && last.command === null) this.remove(last);
    const mark: PromptMark<M> = { marker, command: null, exitCode: null, status: "prompt" };
    this.marks.push(mark);
    this.events.added(mark);
    return mark;
  }

  commandStart(command: string): void {
    const last = this.marks[this.marks.length - 1];
    if (!last || last.command !== null) return;
    last.command = command;
    last.status = "running";
    this.events.changed(last);
  }

  commandEnd(exitCode: number | null): void {
    const last = this.marks[this.marks.length - 1];
    // Not preceded by a command start: the delivery line itself (fish), or a
    // finish whose start was lost.
    if (!last || last.status !== "running") return;
    last.exitCode = exitCode;
    last.status = exitCode === 0 ? "succeeded" : "failed";
    this.events.changed(last);
  }

  /** Forgets marks whose line scrolled out of the buffer. */
  prune(): void {
    for (const mark of [...this.marks]) if (mark.marker.isDisposed || mark.marker.line < 0) this.remove(mark);
  }

  private remove(mark: PromptMark<M>) {
    this.marks = this.marks.filter((other) => other !== mark);
    this.events.removed(mark);
    mark.marker.dispose();
  }

  /**
   * The latest prompt that ran a command, and the line where the prompt after
   * it starts (`null` while it is the last one, so its output runs to the
   * cursor). Used to hand the last command and its output to the assistant.
   */
  lastCommand(): { mark: PromptMark<M>; endLine: number | null } | null {
    for (let i = this.marks.length - 1; i >= 0; i--) {
      const mark = this.marks[i];
      if (mark.command === null || mark.marker.line < 0) continue;
      const next = this.marks[i + 1];
      return { mark, endLine: next && next.marker.line >= 0 ? next.marker.line : null };
    }
    return null;
  }

  /** The buffer line to scroll to for "previous prompt", or null. */
  previous(viewportTop: number): number | null {
    let found: number | null = null;
    for (const mark of this.marks) {
      const line = mark.marker.line;
      if (line >= 0 && line < viewportTop && (found === null || line > found)) found = line;
    }
    return found;
  }

  /** The buffer line for "next prompt", or null when there is none below. */
  next(viewportTop: number): number | null {
    let found: number | null = null;
    for (const mark of this.marks) {
      const line = mark.marker.line;
      if (line > viewportTop && (found === null || line < found)) found = line;
    }
    return found;
  }
}
