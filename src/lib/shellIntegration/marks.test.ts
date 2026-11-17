import { describe, expect, it } from "vitest";
import { PromptMarks, type MarkerLike, type PromptMark } from "./marks";

class FakeMarker implements MarkerLike {
  isDisposed = false;
  constructor(public line: number) {}
  dispose() {
    this.isDisposed = true;
    this.line = -1;
  }
}

function tracker() {
  const log: string[] = [];
  const marks = new PromptMarks({
    added: (mark: PromptMark) => log.push(`add ${mark.marker.line}`),
    changed: (mark: PromptMark) => log.push(`change ${mark.marker.line} ${mark.status}`),
    removed: (mark: PromptMark) => log.push(`remove ${mark.marker.line}`),
  });
  return { marks, log };
}

describe("PromptMarks", () => {
  it("colours a mark by how its command ended", () => {
    const { marks } = tracker();
    const first = marks.promptStart(new FakeMarker(0));
    marks.commandStart("false");
    expect(first.status).toBe("running");
    marks.commandEnd(1);
    expect(first.status).toBe("failed");
    expect(first.exitCode).toBe(1);

    const second = marks.promptStart(new FakeMarker(3));
    marks.commandStart("true");
    marks.commandEnd(0);
    expect(second.status).toBe("succeeded");
    expect(marks.all()).toHaveLength(2);
  });

  it("replaces a prompt that no command was run from", () => {
    const { marks, log } = tracker();
    marks.promptStart(new FakeMarker(0));
    marks.promptStart(new FakeMarker(1)); // Enter on an empty line, or a repaint
    expect(marks.all().map((mark) => mark.marker.line)).toEqual([1]);
    expect(log).toEqual(["add 0", "remove 0", "add 1"]);
  });

  it("ignores a finish that has no start", () => {
    // fish reports the end of the delivery line before the first prompt.
    const { marks } = tracker();
    marks.commandEnd(0);
    const mark = marks.promptStart(new FakeMarker(0));
    marks.commandEnd(0);
    expect(mark.status).toBe("prompt");
  });

  it("forgets marks that scrolled out of the buffer", () => {
    const { marks } = tracker();
    const old = new FakeMarker(0);
    marks.promptStart(old);
    marks.commandStart("a");
    marks.commandEnd(0);
    marks.promptStart(new FakeMarker(5));
    old.dispose();
    marks.prune();
    expect(marks.all().map((mark) => mark.marker.line)).toEqual([5]);
  });

  describe("jumping", () => {
    function filled() {
      const { marks } = tracker();
      for (const line of [2, 10, 20]) {
        marks.promptStart(new FakeMarker(line));
        marks.commandStart("x");
        marks.commandEnd(0);
      }
      return marks;
    }

    it("goes to the prompt above the top of the view", () => {
      const marks = filled();
      expect(marks.previous(15)).toBe(10);
      expect(marks.previous(10)).toBe(2);
      expect(marks.previous(2)).toBeNull();
    });

    it("goes to the prompt below the top of the view", () => {
      const marks = filled();
      expect(marks.next(0)).toBe(2);
      expect(marks.next(2)).toBe(10);
      expect(marks.next(10)).toBe(20);
      expect(marks.next(20)).toBeNull();
    });
  });
});
