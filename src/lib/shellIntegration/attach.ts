import type { IDecoration, IMarker, Terminal } from "@xterm/xterm";
import { t } from "@/lib/i18n";
import { ShellIntegrationLineSource } from "./lineSource";
import { PromptMarks, type PromptMark } from "./marks";
import { INTEGRATION_OSC, parseIntegrationOsc, parseOsc7 } from "./osc";

/** The last command the shell reported, with where its output sits in the buffer. */
export interface ReportedCommand {
  command: string;
  exitCode: number | null;
  /** The buffer line of its prompt. */
  promptLine: number;
  /** The line of the next prompt, or null when this is the latest one. */
  nextPromptLine: number | null;
}

export interface ShellIntegrationHandle {
  /** Scrolls to the previous or next prompt; false when there is none that way. */
  jump(direction: "previous" | "next"): boolean;
  /** The last command run from a marked prompt; null without shell integration. */
  lastCommand(): ReportedCommand | null;
  /** The command still running from the last prompt; null when the shell is
   *  at a prompt or does not report it. */
  running(): string | null;
  dispose(): void;
}

/** The gutter bar's accessible text: the command and how it ended. */
export function markLabel(mark: Pick<PromptMark, "command" | "exitCode" | "status">): string {
  const outcome =
    mark.status === "succeeded"
      ? t.commandSucceeded
      : mark.status === "failed"
        ? `${t.exitCode} ${mark.exitCode ?? ""}`.trim()
        : mark.status === "running"
          ? t.commandRunning
          : "";
  return [mark.command?.trim(), outcome].filter(Boolean).join(" · ");
}

/**
 * Listens for the shell integration's OSC sequence on `terminal`, feeds the
 * line source, and draws one gutter bar per prompt that shows how its command
 * ended (success and danger tokens; see `.prompt-mark` in index.css).
 */
export function attachShellIntegration(
  terminal: Terminal,
  source: ShellIntegrationLineSource,
): ShellIntegrationHandle {
  const decorations = new Map<PromptMark<IMarker>, { decoration: IDecoration; element: HTMLElement | null }>();

  const paint = (mark: PromptMark<IMarker>, element: HTMLElement) => {
    // xterm gives the element its own classes (position); only ours change.
    element.classList.remove("prompt-mark-prompt", "prompt-mark-running", "prompt-mark-succeeded", "prompt-mark-failed");
    element.classList.add("prompt-mark", `prompt-mark-${mark.status}`);
    const label = markLabel(mark);
    if (label) element.title = label;
    else element.removeAttribute("title");
  };

  const marks = new PromptMarks<IMarker>({
    added(mark) {
      const decoration = terminal.registerDecoration({ marker: mark.marker, x: 0, width: 1, height: 1 });
      if (!decoration) return;
      const entry = { decoration, element: null as HTMLElement | null };
      decorations.set(mark, entry);
      decoration.onRender((element) => {
        entry.element = element;
        paint(mark, element);
      });
    },
    changed(mark) {
      const entry = decorations.get(mark);
      if (entry?.element) paint(mark, entry.element);
    },
    removed(mark) {
      decorations.get(mark)?.decoration.dispose();
      decorations.delete(mark);
    },
  });

  const handler = terminal.parser.registerOscHandler(INTEGRATION_OSC, (data) => {
    const event = parseIntegrationOsc(data);
    if (!event) return true;
    source.handle(event);
    if (event.kind === "promptStart") {
      const marker = terminal.registerMarker(0);
      if (marker) marks.promptStart(marker);
    } else if (event.kind === "commandStart") marks.commandStart(event.command);
    else if (event.kind === "commandEnd") marks.commandEnd(event.exitCode);
    return true;
  });
  // A shell without the script may still say where it is (OSC 7).
  const cwdReport = terminal.parser.registerOscHandler(7, (data) => {
    const path = parseOsc7(data);
    if (path) source.handle({ kind: "cwd", path });
    return true;
  });
  const parsed = terminal.onWriteParsed(() => marks.prune());

  return {
    lastCommand() {
      const found = marks.lastCommand();
      if (!found || found.mark.command === null) return null;
      return {
        command: found.mark.command,
        exitCode: found.mark.exitCode,
        promptLine: found.mark.marker.line,
        nextPromptLine: found.endLine,
      };
    },
    running() {
      const last = marks.all().at(-1);
      return last?.status === "running" ? last.command : null;
    },
    jump(direction) {
      const top = terminal.buffer.active.viewportY;
      const line = direction === "previous" ? marks.previous(top) : marks.next(top);
      if (line === null) {
        if (direction === "next") terminal.scrollToBottom();
        return false;
      }
      terminal.scrollToLine(line);
      return true;
    },
    dispose() {
      handler.dispose();
      cwdReport.dispose();
      parsed.dispose();
      for (const { decoration } of decorations.values()) decoration.dispose();
      decorations.clear();
    },
  };
}
