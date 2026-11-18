import { describe, expect, it } from "vitest";
import { diffText, ImeMachine, type ImeEvent, type ImeMode } from "./imeMachine";

const DEL = "\x7f";

/** Plays events into a machine and returns everything sent and the last preedit. */
function play(events: ImeEvent[], mode: ImeMode = "auto", cjk = false) {
  const machine = new ImeMachine(mode, cjk);
  let sent = "";
  let preedit = "";
  const steps: string[] = [];
  for (const event of events) {
    const result = machine.feed(event);
    sent += result.send;
    preedit = result.preedit;
    steps.push(result.send);
  }
  return { sent, preedit, steps, machine };
}

const start: ImeEvent = { type: "compositionstart" };
const update = (data: string): ImeEvent => ({ type: "compositionupdate", data });
const end = (data: string): ImeEvent => ({ type: "compositionend", data });
const insert = (data: string): ImeEvent => ({ type: "insert", data });

describe("diffText", () => {
  it("keeps the shared prefix and replaces the rest", () => {
    expect(diffText("", "ls")).toBe("ls");
    expect(diffText("ls", "ls -")).toBe("ls -".slice(2));
    expect(diffText("teh", "the")).toBe(`${DEL}${DEL}he`);
    expect(diffText("abc", "")).toBe(DEL.repeat(3));
    expect(diffText("same", "same")).toBe("");
  });

  it("counts characters, not UTF-16 units", () => {
    expect(diffText("a😀", "a")).toBe(DEL);
    expect(diffText("你好", "你们")).toBe(`${DEL}们`);
  });
});

describe("Gboard English: predictive text composes each word", () => {
  it("sends the letters as they are typed and nothing twice at the end", () => {
    const { sent, steps } = play([start, update("l"), update("ls"), end("ls"), insert(" ")]);
    expect(steps).toEqual(["", "l", "s", "", " "]);
    expect(sent).toBe("ls ");
  });

  it("corrects the word with backspaces when the keyboard replaces it on commit", () => {
    const { sent } = play([start, update("t"), update("te"), update("teh"), end("the"), insert(" ")]);
    // t, e, h typed, then "teh" becomes "the": two DEL and "he".
    expect(sent).toBe(`teh${DEL}${DEL}he `);
  });

  it("drops the duplicate insert some keyboards send after the commit", () => {
    const { sent } = play([start, update("git"), end("git"), insert("git"), insert(" ")]);
    expect(sent).toBe("git ");
  });

  it("does not drop a different insert after a commit, nor the same one later", () => {
    const { sent } = play([start, update("a"), end("a"), insert("b"), insert("a")]);
    expect(sent).toBe("aba");
  });

  it("forgets the duplicate guard after any key press", () => {
    const { sent } = play([start, update("a"), end("a"), { type: "other" }, insert("a")]);
    expect(sent).toBe("aa");
  });

  it("handles composition update without a start event", () => {
    const { sent } = play([update("ab"), end("ab")]);
    expect(sent).toBe("ab");
  });

  it("deleting inside the word arrives as updates, not as backspace events", () => {
    const { sent } = play([start, update("lss"), { type: "backspace" }, update("ls"), end("ls")]);
    expect(sent).toBe(`lss${DEL}`);
  });

  it("an empty commit (the word was deleted) sends the clean-up only", () => {
    const { sent } = play([start, update("a"), update(""), end("")]);
    expect(sent).toBe(`a${DEL}`);
  });
});

describe("Gboard Chinese pinyin", () => {
  it("shows the pinyin as a preedit and sends only the committed hanzi (CJK locale)", () => {
    const { sent, steps } = play(
      [start, update("n"), update("ni"), update("ni'h"), update("ni'hao"), end("你好")],
      "auto",
      true,
    );
    expect(steps).toEqual(["", "", "", "", "", "你好"]);
    expect(sent).toBe("你好");
  });

  it("the preedit is reported while composing and cleared at the end", () => {
    const machine = new ImeMachine("auto", true);
    machine.feed(start);
    expect(machine.feed(update("ni")).preedit).toBe("ni");
    expect(machine.feed(update("ni hao")).preedit).toBe("ni hao");
    expect(machine.feed(end("你好")).preedit).toBe("");
  });

  it("recognises pinyin on a non-CJK locale from the space and takes back the letters", () => {
    const { sent } = play([start, update("n"), update("ni"), update("ni h"), update("ni hao"), end("你好")]);
    // "n", "i" went out live; at "ni h" they are taken back, then the commit.
    expect(sent).toBe(`ni${DEL}${DEL}你好`);
  });

  it("a candidate picked in two steps commits twice", () => {
    const { sent } = play(
      [start, update("ni"), end("你"), start, update("hao"), end("好")],
      "auto",
      true,
    );
    expect(sent).toBe("你好");
  });

  it("the committed hanzi is not repeated by a trailing insert", () => {
    const { sent } = play([start, update("ni"), end("你"), insert("你")], "auto", true);
    expect(sent).toBe("你");
  });
});

describe("iOS keyboard", () => {
  it("types plain letters as inserts, one per key, with no composition", () => {
    const { sent } = play([insert("l"), insert("s"), insert(" "), insert("-"), insert("l")]);
    expect(sent).toBe("ls -l");
  });

  it("iOS pinyin: preedit with tone marks stays out of the shell", () => {
    const { sent, preedit } = play([start, update("n"), update("nǐ"), update("nǐ hǎo")], "auto", false);
    // "n" went out live before the tone mark showed it was an IME; it was taken back.
    expect(sent).toBe(`n${DEL}`);
    expect(preedit).toBe("nǐ hǎo");
  });

  it("iOS pinyin commit", () => {
    const { sent, preedit } = play([start, update("nǐ hǎo"), end("你好")]);
    expect(sent).toBe("你好");
    expect(preedit).toBe("");
  });

  it("iOS Japanese romaji: kana preedit, then the kanji", () => {
    const { sent } = play([start, update("n"), update("に"), update("にほ"), update("にほん"), end("日本")]);
    expect(sent).toBe(`n${DEL}日本`);
  });

  it("Korean hangul composes per syllable", () => {
    const { sent } = play([start, update("ㅎ"), update("하"), update("한"), end("한"), start, update("ㄱ"), update("글"), end("글")]);
    expect(sent).toBe("한글");
  });
});

describe("modes", () => {
  it("hold never sends before the commit, even for plain letters", () => {
    const { sent, steps } = play([start, update("ls"), end("ls")], "hold");
    expect(steps).toEqual(["", "", "ls"]);
    expect(sent).toBe("ls");
  });

  it("live never holds, even with a space or non-ASCII", () => {
    const { sent } = play([start, update("a b"), end("a b")], "live");
    expect(sent).toBe("a b");
  });

  it("auto holds every composition on a CJK locale, including English words", () => {
    const { steps } = play([start, update("ls"), end("ls")], "auto", true);
    expect(steps).toEqual(["", "", "ls"]);
  });

  it("the mode can change between compositions", () => {
    const machine = new ImeMachine("live", false);
    expect(machine.feed(start).send).toBe("");
    expect(machine.feed(update("a")).send).toBe("a");
    machine.feed(end("a"));
    machine.configure("hold", false);
    machine.feed(start);
    expect(machine.feed(update("b")).send).toBe("");
  });
});

describe("keys outside a composition", () => {
  it("backspace on an empty field still sends DEL", () => {
    const { sent } = play([{ type: "backspace" }, { type: "backspace" }]);
    expect(sent).toBe(`${DEL}${DEL}`);
  });

  it("Enter, word delete and inserted line breaks", () => {
    expect(play([{ type: "enter" }]).sent).toBe("\r");
    expect(play([{ type: "deleteWord" }]).sent).toBe("\x17");
    expect(play([insert("a\nb")]).sent).toBe("a\rb");
    expect(play([insert("a\r\nb")]).sent).toBe("a\rb");
  });

  it("an insert while composing belongs to the composition", () => {
    const { sent } = play([start, update("a"), insert("a"), end("a")], "live");
    expect(sent).toBe("a");
  });

  it("Enter during a composition is not sent (the keyboard commits first)", () => {
    const machine = new ImeMachine("hold", false);
    machine.feed(start);
    expect(machine.feed({ type: "enter" }).send).toBe("");
  });

  it("reports whether a composition is running", () => {
    const machine = new ImeMachine();
    expect(machine.isComposing).toBe(false);
    machine.feed(start);
    expect(machine.isComposing).toBe(true);
    machine.feed(end(""));
    expect(machine.isComposing).toBe(false);
  });
});
