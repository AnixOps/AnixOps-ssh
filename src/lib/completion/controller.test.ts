import { describe, expect, it } from "vitest";
import { CompletionController } from "./controller";
import { HistoryStore } from "./history";
import { KeystrokeLineSource } from "./lineSource";
import { commandNames, loadSpec } from "./specs";

/** A terminal that echoes what is written into it. */
function setup(enabled = true) {
  const cursor = { x: 0, y: 0, absoluteY: 0, cols: 80 };
  const sent: string[] = [];
  const history = new HistoryStore(null);
  const controller = new CompletionController({
    hostId: "h",
    history,
    deps: { loadSpec, commandNames, history },
    cursor: () => ({ ...cursor }),
    write: (data) => sent.push(data),
    enabled: () => enabled,
    source: new KeystrokeLineSource(() => ({ x: cursor.x, y: cursor.absoluteY, cols: cursor.cols })),
  });
  const type = async (text: string) => {
    controller.input(text);
    cursor.x += [...text].length;
    controller.outputParsed();
    // Let the asynchronous suggestion finish.
    await new Promise((resolve) => setTimeout(resolve, 20));
  };
  return { controller, sent, history, type };
}

describe("CompletionController", () => {
  it("shows suggestions while typing and does not steal Enter", async () => {
    const { controller, type, sent } = setup();
    await type("git com");
    const popup = controller.getView().popup;
    expect(popup?.items[0].label).toBe("commit");
    expect(controller.handleKey({ key: "Enter" })).toBe(false);
    expect(sent).toEqual([]);
  });

  it("accepts the highlighted item with Down then Enter", async () => {
    const { controller, type, sent } = setup();
    await type("git com");
    expect(controller.handleKey({ key: "ArrowDown" })).toBe(true);
    expect(controller.handleKey({ key: "Enter" })).toBe(true);
    expect(sent).toEqual(["mit "]);
    expect(controller.getView().popup).toBeNull();
  });

  it("Tab accepts the first item and Escape hides the list until the line changes", async () => {
    const first = setup();
    await first.type("git sta");
    expect(first.controller.handleKey({ key: "Tab" })).toBe(true);
    expect(first.sent[0]).toMatch(/^(tus|sh) $/);

    const second = setup();
    await second.type("git com");
    expect(second.controller.handleKey({ key: "Escape" })).toBe(true);
    expect(second.controller.getView().popup).toBeNull();
    second.controller.outputParsed();
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(second.controller.getView().popup).toBeNull();
    await second.type("m");
    expect(second.controller.getView().popup).not.toBeNull();
  });

  it("hides when the line becomes unknown", async () => {
    const { controller, type } = setup();
    await type("git com");
    controller.input("\x1b[D");
    controller.outputParsed();
    expect(controller.getView().popup).toBeNull();
  });

  it("stays out of the way when disabled", async () => {
    const { controller, type } = setup(false);
    await type("git com");
    expect(controller.getView().popup).toBeNull();
    expect(controller.handleKey({ key: "Tab" })).toBe(false);
  });

  it("records commands that were run, per host", async () => {
    const { controller, type, history } = setup();
    await type("ls -la");
    controller.input("\r");
    expect(history.recent("h")).toEqual(["ls -la"]);
  });

  it("hides in the alternate screen", async () => {
    const { controller, type } = setup();
    await type("git com");
    controller.setAlternateScreen(true);
    expect(controller.getView().popup).toBeNull();
  });
});
