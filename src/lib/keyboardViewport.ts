import { useEffect, useState } from "react";

/* The part of the screen the app can use while the soft keyboard is up.

   Android may shrink the page when the keyboard opens (then `innerHeight`
   shrinks with the visual viewport) or leave it alone; iOS never shrinks the
   page, it only reports a smaller visual viewport and may pan it
   (`offsetTop`). In both cases the app is laid out to the visual viewport, so
   the key bar sits directly above the keyboard, and it knows whether a
   keyboard is up so it can drop the bottom safe-area padding the keyboard
   already covers. */

export interface ViewportInput {
  innerWidth: number;
  innerHeight: number;
  /** `visualViewport.height`, `.offsetTop`; the inner size when the API is missing. */
  height: number;
  offsetTop: number;
}

export interface ViewportState {
  /** Where the visible area starts and how tall it is. */
  top: number;
  height: number;
  /** The keyboard (or anything else) covers this much of the page. */
  inset: number;
  keyboardOpen: boolean;
  /** The tallest page height seen in this orientation: what "no keyboard" looks like. */
  reference: number;
  portrait: boolean;
  /** The page width the reference belongs to: a keyboard never changes it, a rotation does. */
  width: number;
}

/** Below this the change is a browser toolbar, not a keyboard. */
export const KEYBOARD_MIN_PX = 120;

export function initialViewport(input: ViewportInput): ViewportState {
  return nextViewport(null, input);
}

export function nextViewport(previous: ViewportState | null, input: ViewportInput): ViewportState {
  const portrait = input.innerHeight >= input.innerWidth;
  // A rotation starts a new reference; within one orientation the page is at
  // its tallest when no keyboard is up.
  // (Not by comparing height with width: a 390 px wide phone with a keyboard
  // up is 364 px tall and would look like landscape.)
  const sameOrientation = previous !== null && previous.width === input.innerWidth;
  const reference = sameOrientation ? Math.max(previous.reference, input.innerHeight) : input.innerHeight;
  const visibleBottom = input.height + input.offsetTop;
  const inset = Math.max(0, Math.round(reference - visibleBottom));
  return {
    top: input.offsetTop,
    height: input.height,
    inset,
    keyboardOpen: inset >= KEYBOARD_MIN_PX,
    reference,
    portrait,
    width: input.innerWidth,
  };
}

function readInput(): ViewportInput {
  const viewport = window.visualViewport;
  return {
    innerWidth: window.innerWidth,
    innerHeight: window.innerHeight,
    height: viewport?.height ?? window.innerHeight,
    offsetTop: viewport?.offsetTop ?? 0,
  };
}

/** The visible area, kept up to date as the keyboard opens and closes and the
 *  phone rotates. Sets `data-keyboard` on the root element too. */
export function useKeyboardViewport(): ViewportState {
  const [state, setState] = useState<ViewportState>(() => initialViewport(readInput()));
  useEffect(() => {
    const viewport = window.visualViewport;
    const update = () => {
      setState((previous) => nextViewport(previous, readInput()));
      // iOS scrolls the page to keep the focused field in view; the layout
      // already fits the visual viewport, so put the page back.
      if (window.scrollY !== 0) window.scrollTo(0, 0);
    };
    viewport?.addEventListener("resize", update);
    viewport?.addEventListener("scroll", update);
    window.addEventListener("resize", update);
    window.addEventListener("orientationchange", update);
    return () => {
      viewport?.removeEventListener("resize", update);
      viewport?.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
      window.removeEventListener("orientationchange", update);
    };
  }, []);
  useEffect(() => {
    document.documentElement.dataset.keyboard = state.keyboardOpen ? "open" : "closed";
    return () => {
      delete document.documentElement.dataset.keyboard;
    };
  }, [state.keyboardOpen]);
  return state;
}
