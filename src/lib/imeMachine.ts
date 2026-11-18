/* Soft-keyboard input for a terminal, as a state machine with no DOM in it.

   xterm.js reads typing from a hidden textarea. On a phone that textarea does
   not see key presses: the keyboard (Gboard, the iOS keyboard, a Chinese or
   Japanese IME) edits "composition" text in it and reports it with
   compositionstart / compositionupdate / compositionend and beforeinput
   events, and a terminal has to turn that into bytes without losing,
   duplicating or reordering characters. xterm.js's own handling diffs the
   textarea after a timeout, which misfires with predictive text. This machine
   replaces it; `imeLayer.ts` feeds it from the real events.

   Two ways to treat a composition:

   - live: the characters go to the shell as they are typed, and when the
     keyboard changes the word (auto-correct, a suggestion) the difference is
     sent as backspaces and the new tail. This is what an English predictive
     keyboard needs; the shell sees every key.
   - hold: nothing goes to the shell until the composition ends, then the
     committed text goes in one piece. This is what pinyin, kana or hangul
     need: the preedit ("ni hao", "にほ") is not text for the shell. The
     preedit is shown in a small chip instead.

   `auto` is live for compositions of plain letters and switches to hold the
   moment one contains a space or a non-ASCII character (or always for a CJK
   locale), taking back what was already sent. */

export type ImeMode = "auto" | "live" | "hold";

export type ImeEvent =
  | { type: "compositionstart" }
  | { type: "compositionupdate"; data: string }
  | { type: "compositionend"; data: string }
  /** Text inserted outside a composition (beforeinput insertText). */
  | { type: "insert"; data: string }
  /** Backspace outside a composition. */
  | { type: "backspace" }
  | { type: "deleteWord" }
  | { type: "enter" }
  /** Anything else that happened (a key press), so a stale duplicate is not matched. */
  | { type: "other" };

export interface ImeResult {
  /** Bytes for the shell, possibly empty. */
  send: string;
  /** Uncommitted composition text to show, empty when none. */
  preedit: string;
}

const DEL = "\x7f";

function points(text: string): string[] {
  return [...text];
}

/** Backspaces and text that turn `from` into `to` (a shared prefix stays). */
export function diffText(from: string, to: string): string {
  const a = points(from);
  const b = points(to);
  let common = 0;
  while (common < a.length && common < b.length && a[common] === b[common]) common++;
  return DEL.repeat(a.length - common) + b.slice(common).join("");
}

function needsPreedit(data: string): boolean {
  // eslint-disable-next-line no-control-regex
  return data.includes(" ") || /[^\x00-\x7f]/.test(data);
}

export class ImeMachine {
  private composing = false;
  private held = false;
  /** What this composition has already sent to the shell (live mode). */
  private sent = "";
  /** The last committed text, to drop the duplicate some keyboards add. */
  private committed: string | null = null;

  /** `cjk`: the person's language is Chinese, Japanese or Korean, so every
   *  composition is held in `auto` mode. */
  constructor(
    private mode: ImeMode = "auto",
    private cjk = false,
  ) {}

  configure(mode: ImeMode, cjk: boolean) {
    this.mode = mode;
    this.cjk = cjk;
  }

  get isComposing(): boolean {
    return this.composing;
  }

  private baseHold(): boolean {
    return this.mode === "hold" || (this.mode === "auto" && this.cjk);
  }

  private start() {
    this.composing = true;
    this.held = this.baseHold();
    this.sent = "";
    this.committed = null;
  }

  feed(event: ImeEvent): ImeResult {
    switch (event.type) {
      case "compositionstart":
        this.start();
        return { send: "", preedit: "" };

      case "compositionupdate": {
        if (!this.composing) this.start();
        const data = event.data;
        let send = "";
        if (!this.held && this.mode === "auto" && needsPreedit(data)) {
          // It is an IME's preedit, not a word: take back what was sent.
          send = DEL.repeat(points(this.sent).length);
          this.sent = "";
          this.held = true;
        }
        if (this.held) return { send, preedit: data };
        send += diffText(this.sent, data);
        this.sent = data;
        return { send, preedit: "" };
      }

      case "compositionend": {
        if (!this.composing) this.start();
        const final = event.data;
        const send = this.held ? final : diffText(this.sent, final);
        this.composing = false;
        this.held = false;
        this.sent = "";
        this.committed = final === "" ? null : final;
        return { send, preedit: "" };
      }

      case "insert": {
        // Some keyboards repeat the committed text as an insert right after
        // the composition ended; that is the same text, not new typing.
        if (this.committed !== null && event.data === this.committed) {
          this.committed = null;
          return { send: "", preedit: "" };
        }
        this.committed = null;
        // Inserts during a composition belong to the composition events.
        if (this.composing) return { send: "", preedit: "" };
        return { send: event.data.replace(/\r?\n/g, "\r"), preedit: "" };
      }

      case "backspace":
        this.committed = null;
        // While composing, a delete edits the preedit; an update follows.
        return { send: this.composing ? "" : DEL, preedit: "" };

      case "deleteWord":
        this.committed = null;
        return { send: this.composing ? "" : "\x17", preedit: "" };

      case "enter":
        this.committed = null;
        return { send: this.composing ? "" : "\r", preedit: "" };

      case "other":
        this.committed = null;
        return { send: "", preedit: "" };
    }
  }
}
