import { ImeMachine, type ImeEvent, type ImeMode, type ImeResult } from "./imeMachine";

/* Connects xterm.js's hidden textarea to the ImeMachine.

   The listeners sit on the terminal's container in the capture phase and stop
   the events they handle, so xterm.js never sees them: its own composition
   and input handling would send the same text a second time.

   The textarea always holds one space (the sentinel). A soft keyboard that
   finds the field empty sends no event at all for Backspace (Gboard) or none
   that can be told from nothing; with a character before the cursor it
   reports a deletion, which is sent as DEL and then cancelled, so the
   sentinel stays. Hardware keys do not go through here: xterm.js handles
   keydown and keypress as before. */

export const SENTINEL = " ";

export interface ImeLayer {
  machine: ImeMachine;
  configure(mode: ImeMode, cjk: boolean): void;
  dispose(): void;
}

export function attachImeLayer(
  textarea: HTMLTextAreaElement,
  host: HTMLElement,
  options: {
    mode: ImeMode;
    cjk: boolean;
    send: (data: string) => void;
    onPreedit: (text: string) => void;
  },
): ImeLayer {
  const machine = new ImeMachine(options.mode, options.cjk);
  let lastPreedit = "";

  const apply = (result: ImeResult) => {
    if (result.send) options.send(result.send);
    if (result.preedit !== lastPreedit) {
      lastPreedit = result.preedit;
      options.onPreedit(result.preedit);
    }
  };

  const ensureSentinel = () => {
    if (machine.isComposing || textarea.value === SENTINEL) return;
    textarea.value = SENTINEL;
    try {
      textarea.setSelectionRange(SENTINEL.length, SENTINEL.length);
    } catch {
      // Not focusable yet; the next focus sets it again.
    }
  };

  const fromTextarea = (event: Event) => event.target === textarea;
  const compose = (type: "compositionstart" | "compositionupdate" | "compositionend") => (event: Event) => {
    if (!fromTextarea(event)) return;
    event.stopPropagation();
    const data = (event as CompositionEvent).data ?? "";
    apply(machine.feed(type === "compositionstart" ? { type } : { type, data }));
    if (type === "compositionend") window.setTimeout(ensureSentinel, 0);
  };

  const beforeInput = (event: Event) => {
    if (!fromTextarea(event)) return;
    const input = event as InputEvent;
    const kind = input.inputType;
    // The composition's own edits (insertCompositionText and the deletions
    // inside it) are reported by the composition events.
    if (machine.isComposing || input.isComposing || kind === "insertCompositionText") {
      event.stopPropagation();
      return;
    }
    let next: ImeEvent | null = null;
    if (kind === "insertText" || kind === "insertReplacementText") {
      // A replacement (autocorrect) names text but not the range it replaces;
      // with autocorrect off it should not occur, and typing it again would
      // duplicate the word, so it is dropped.
      next = kind === "insertText" && input.data ? { type: "insert", data: input.data } : null;
    } else if (kind === "deleteContentBackward") next = { type: "backspace" };
    else if (kind === "deleteWordBackward") next = { type: "deleteWord" };
    else if (kind === "insertLineBreak" || kind === "insertParagraph") next = { type: "enter" };
    else return; // paste, drop, undo ...: xterm.js's paste handling takes those
    event.stopPropagation();
    if (input.cancelable) event.preventDefault();
    if (next) apply(machine.feed(next));
    window.setTimeout(ensureSentinel, 0);
  };

  // xterm.js's `input` handler would send insertText data again.
  const input = (event: Event) => {
    if (!fromTextarea(event)) return;
    const kind = (event as InputEvent).inputType;
    if (
      machine.isComposing ||
      (event as InputEvent).isComposing ||
      kind === "insertText" ||
      kind === "insertCompositionText" ||
      kind === "deleteContentBackward" ||
      kind === "deleteWordBackward" ||
      kind === "insertLineBreak" ||
      kind === "insertParagraph"
    ) {
      event.stopPropagation();
    }
    window.setTimeout(ensureSentinel, 0);
  };

  const keydown = (event: Event) => {
    if (!fromTextarea(event)) return;
    const key = event as KeyboardEvent;
    // The keyboard is handling this key itself (keyCode 229 and key
    // "Process" or "Unidentified" mean "an IME has it"): the composition and
    // beforeinput events carry the result.
    if (key.isComposing || key.keyCode === 229 || key.key === "Process" || key.key === "Unidentified") {
      event.stopPropagation();
      return;
    }
    apply(machine.feed({ type: "other" }));
  };

  const focus = () => {
    ensureSentinel();
  };

  const listeners: [string, EventListener][] = [
    ["compositionstart", compose("compositionstart")],
    ["compositionupdate", compose("compositionupdate")],
    ["compositionend", compose("compositionend")],
    ["beforeinput", beforeInput],
    ["input", input],
    ["keydown", keydown],
  ];
  for (const [name, listener] of listeners) host.addEventListener(name, listener, true);
  textarea.addEventListener("focus", focus);
  textarea.addEventListener("keyup", focus);
  ensureSentinel();

  return {
    machine,
    configure(mode, cjk) {
      machine.configure(mode, cjk);
    },
    dispose() {
      for (const [name, listener] of listeners) host.removeEventListener(name, listener, true);
      textarea.removeEventListener("focus", focus);
      textarea.removeEventListener("keyup", focus);
    },
  };
}
