import { describe, expect, it } from "vitest";
import { initialViewport, nextViewport, type ViewportInput, type ViewportState } from "./keyboardViewport";

const input = (
  innerWidth: number,
  innerHeight: number,
  height = innerHeight,
  offsetTop = 0,
): ViewportInput => ({ innerWidth, innerHeight, height, offsetTop });

function run(inputs: ViewportInput[]): ViewportState[] {
  const states: ViewportState[] = [];
  let state: ViewportState | null = null;
  for (const next of inputs) {
    state = nextViewport(state, next);
    states.push(state);
  }
  return states;
}

describe("soft keyboard", () => {
  it("starts with no keyboard", () => {
    const state = initialViewport(input(390, 844));
    expect(state).toMatchObject({ top: 0, height: 844, inset: 0, keyboardOpen: false, portrait: true });
  });

  it("Android that resizes the page: both heights shrink, the inset is the difference", () => {
    const [closed, open, closedAgain] = run([input(412, 915), input(412, 600, 600), input(412, 915)]);
    expect(closed.keyboardOpen).toBe(false);
    expect(open).toMatchObject({ height: 600, inset: 315, keyboardOpen: true });
    expect(closedAgain).toMatchObject({ inset: 0, keyboardOpen: false });
  });

  it("iOS that does not resize the page: only the visual viewport shrinks", () => {
    const [closed, open] = run([input(390, 844), input(390, 844, 508)]);
    expect(closed.inset).toBe(0);
    expect(open).toMatchObject({ height: 508, inset: 336, keyboardOpen: true });
  });

  it("iOS that pans the visual viewport counts the offset", () => {
    const [, open] = run([input(390, 844), input(390, 844, 508, 120)]);
    expect(open).toMatchObject({ top: 120, height: 508, inset: 216, keyboardOpen: true });
  });

  it("a browser toolbar sliding in is not a keyboard", () => {
    const [, bar] = run([input(412, 915), input(412, 915, 870)]);
    expect(bar.inset).toBe(45);
    expect(bar.keyboardOpen).toBe(false);
  });

  it("a page that starts with the keyboard already open learns the reference when it closes", () => {
    const [open, closed] = run([input(412, 600, 600), input(412, 915)]);
    expect(open.keyboardOpen).toBe(false);
    expect(closed.keyboardOpen).toBe(false);
    expect(closed.reference).toBe(915);
  });
});

describe("rotation and tablets", () => {
  it("rotating starts a new reference for the new orientation", () => {
    const [portrait, landscape, landscapeKeyboard, back] = run([
      input(390, 844),
      input(844, 390),
      input(844, 390, 160),
      input(390, 844),
    ]);
    expect(portrait.portrait).toBe(true);
    expect(landscape).toMatchObject({ portrait: false, reference: 390, inset: 0 });
    expect(landscapeKeyboard).toMatchObject({ inset: 230, keyboardOpen: true });
    expect(back).toMatchObject({ portrait: true, reference: 844, inset: 0, keyboardOpen: false });
  });

  it("a keyboard open during rotation does not make the shorter page the reference forever", () => {
    // Rotated while the keyboard was up (Android resizes): 390 is really 160 + keyboard.
    const [, landscape, closed] = run([input(390, 844), input(844, 160, 160), input(844, 390)]);
    expect(landscape.keyboardOpen).toBe(false);
    expect(closed.reference).toBe(390);
  });

  it("a keyboard that makes a narrow phone's page shorter than it is wide is not a rotation", () => {
    const [, open, closed] = run([input(390, 664), input(390, 364), input(390, 664)]);
    expect(open).toMatchObject({ inset: 300, keyboardOpen: true, reference: 664 });
    expect(closed).toMatchObject({ inset: 0, keyboardOpen: false });
  });

  it("a tablet with the keyboard docked at the bottom", () => {
    const [closed, open] = run([input(1024, 768), input(1024, 768, 468)]);
    expect(closed.keyboardOpen).toBe(false);
    expect(open).toMatchObject({ inset: 300, keyboardOpen: true });
  });

  it("works without a visual viewport API (heights equal the inner size)", () => {
    const [state] = run([input(800, 600)]);
    expect(state).toMatchObject({ height: 600, top: 0, inset: 0 });
  });
});
