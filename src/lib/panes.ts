/* The pane layout of one tab: a binary tree whose leaves are panes and whose
   inner nodes split their area in two. Everything here is pure, so the
   layout rules can be tested without a browser. */

export type Axis = "row" | "col"; // row: side by side; col: stacked
export type Direction = "left" | "right" | "up" | "down";

export type PaneNode =
  | { type: "leaf"; id: string }
  | { type: "split"; id: string; axis: Axis; ratio: number; a: PaneNode; b: PaneNode };

export const MIN_RATIO = 0.1;
export const MAX_RATIO = 0.9;

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export function clampRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return 0.5;
  return Math.min(MAX_RATIO, Math.max(MIN_RATIO, ratio));
}

export function leaf(id: string): PaneNode {
  return { type: "leaf", id };
}

/** Pane ids from left/top to right/bottom. */
export function leaves(node: PaneNode): string[] {
  return node.type === "leaf" ? [node.id] : [...leaves(node.a), ...leaves(node.b)];
}

/** Replaces the pane `id` by a split holding it and a new pane. */
export function splitPane(
  node: PaneNode,
  id: string,
  axis: Axis,
  newPaneId: string,
  splitId: string,
): PaneNode {
  if (node.type === "leaf") {
    if (node.id !== id) return node;
    return { type: "split", id: splitId, axis, ratio: 0.5, a: node, b: leaf(newPaneId) };
  }
  return {
    ...node,
    a: splitPane(node.a, id, axis, newPaneId, splitId),
    b: splitPane(node.b, id, axis, newPaneId, splitId),
  };
}

/** Removes a pane; its sibling takes the space. Null when the last pane goes. */
export function closePane(node: PaneNode, id: string): PaneNode | null {
  if (node.type === "leaf") return node.id === id ? null : node;
  const a = closePane(node.a, id);
  const b = closePane(node.b, id);
  if (a === null) return b;
  if (b === null) return a;
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

/** Sets the divider position of a split, clamped so no pane vanishes. */
export function resizeSplit(node: PaneNode, splitId: string, ratio: number): PaneNode {
  if (node.type === "leaf") return node;
  if (node.id === splitId) return { ...node, ratio: clampRatio(ratio) };
  return { ...node, a: resizeSplit(node.a, splitId, ratio), b: resizeSplit(node.b, splitId, ratio) };
}

export interface Divider {
  id: string;
  axis: Axis;
  ratio: number;
  /** The area of the whole split, in unit coordinates. */
  area: Rect;
}

export interface Layout {
  panes: Record<string, Rect>;
  dividers: Divider[];
}

/** Unit-square geometry of every pane and divider. */
export function layout(node: PaneNode, area: Rect = { x: 0, y: 0, w: 1, h: 1 }): Layout {
  if (node.type === "leaf") return { panes: { [node.id]: area }, dividers: [] };
  const first: Rect =
    node.axis === "row" ? { ...area, w: area.w * node.ratio } : { ...area, h: area.h * node.ratio };
  const second: Rect =
    node.axis === "row"
      ? { x: area.x + first.w, y: area.y, w: area.w - first.w, h: area.h }
      : { x: area.x, y: area.y + first.h, w: area.w, h: area.h - first.h };
  const a = layout(node.a, first);
  const b = layout(node.b, second);
  return {
    panes: { ...a.panes, ...b.panes },
    dividers: [{ id: node.id, axis: node.axis, ratio: node.ratio, area }, ...a.dividers, ...b.dividers],
  };
}

/** The next (or previous) pane in reading order, wrapping around. */
export function cyclePane(node: PaneNode, id: string, step: 1 | -1): string {
  const order = leaves(node);
  const index = order.indexOf(id);
  if (index < 0) return order[0];
  return order[(index + step + order.length) % order.length];
}

/** The neighbouring pane in a direction, by geometry; the same pane if none. */
export function focusDirection(node: PaneNode, id: string, direction: Direction): string {
  const { panes } = layout(node);
  const from = panes[id];
  if (!from) return id;
  const centre = { x: from.x + from.w / 2, y: from.y + from.h / 2 };
  const eps = 1e-9;
  let best: { id: string; score: number } | null = null;
  for (const [other, rect] of Object.entries(panes)) {
    if (other === id) continue;
    const middle = { x: rect.x + rect.w / 2, y: rect.y + rect.h / 2 };
    let ahead: number;
    let aside: number;
    let overlaps: boolean;
    if (direction === "left" || direction === "right") {
      ahead = direction === "right" ? rect.x - (from.x + from.w) : from.x - (rect.x + rect.w);
      aside = Math.abs(middle.y - centre.y);
      overlaps = rect.y < from.y + from.h - eps && rect.y + rect.h > from.y + eps;
    } else {
      ahead = direction === "down" ? rect.y - (from.y + from.h) : from.y - (rect.y + rect.h);
      aside = Math.abs(middle.x - centre.x);
      overlaps = rect.x < from.x + from.w - eps && rect.x + rect.w > from.x + eps;
    }
    if (ahead < -eps || !overlaps) continue;
    const score = Math.max(ahead, 0) * 4 + aside;
    if (best === null || score < best.score) best = { id: other, score };
  }
  return best?.id ?? id;
}

/** After closing `id`, the pane that should take focus: its predecessor in order. */
export function focusAfterClose(before: PaneNode, id: string): string | null {
  const order = leaves(before);
  const index = order.indexOf(id);
  const rest = order.filter((pane) => pane !== id);
  if (rest.length === 0) return null;
  return rest[Math.min(Math.max(index - 1, 0), rest.length - 1)];
}

/* Persistence: pane ids are runtime names, so the saved form keeps only the
   shape and the daemon session behind each pane. */

export type SavedNode =
  | { t: "leaf"; session: string | null }
  | { t: "split"; axis: Axis; ratio: number; a: SavedNode; b: SavedNode };

export function serialize(node: PaneNode, sessionOf: (paneId: string) => string | null): SavedNode {
  if (node.type === "leaf") return { t: "leaf", session: sessionOf(node.id) };
  return {
    t: "split",
    axis: node.axis,
    ratio: node.ratio,
    a: serialize(node.a, sessionOf),
    b: serialize(node.b, sessionOf),
  };
}

/** Rebuilds a tree with fresh ids; `sessions` maps each new pane to its saved session. */
export function deserialize(
  saved: SavedNode,
  newId: () => string,
): { tree: PaneNode; sessions: Record<string, string | null> } {
  const sessions: Record<string, string | null> = {};
  const build = (node: SavedNode): PaneNode => {
    if (node.t === "leaf") {
      const id = newId();
      sessions[id] = node.session;
      return leaf(id);
    }
    return {
      type: "split",
      id: newId(),
      axis: node.axis === "col" ? "col" : "row",
      ratio: clampRatio(node.ratio),
      a: build(node.a),
      b: build(node.b),
    };
  };
  return { tree: build(saved), sessions };
}

/** Whether untrusted JSON has the shape of a saved tree. */
export function isSavedNode(value: unknown, depth = 0): value is SavedNode {
  if (depth > 16 || typeof value !== "object" || value === null) return false;
  const node = value as Record<string, unknown>;
  if (node.t === "leaf") return node.session === null || typeof node.session === "string";
  if (node.t === "split") {
    return (
      (node.axis === "row" || node.axis === "col") &&
      typeof node.ratio === "number" &&
      isSavedNode(node.a, depth + 1) &&
      isSavedNode(node.b, depth + 1)
    );
  }
  return false;
}

/** Drops leaves whose session is gone (the daemon no longer knows it). Null when nothing is left. */
export function pruneSaved(node: SavedNode, alive: (session: string) => boolean): SavedNode | null {
  if (node.t === "leaf") return node.session !== null && alive(node.session) ? node : null;
  const a = pruneSaved(node.a, alive);
  const b = pruneSaved(node.b, alive);
  if (a === null) return b;
  if (b === null) return a;
  return { ...node, a, b };
}

export function savedSessions(node: SavedNode): string[] {
  if (node.t === "leaf") return node.session === null ? [] : [node.session];
  return [...savedSessions(node.a), ...savedSessions(node.b)];
}

/* The state of one tab's panes. */

export interface PaneState {
  tree: PaneNode;
  focused: string;
  /** A pane shown alone, filling the tab; the others keep running unseen. */
  maximised: string | null;
}

export function initialState(paneId: string): PaneState {
  return { tree: leaf(paneId), focused: paneId, maximised: null };
}

/** Splits the focused pane; the new pane takes focus. */
export function splitFocused(state: PaneState, axis: Axis, newPaneId: string, splitId: string): PaneState {
  return {
    tree: splitPane(state.tree, state.focused, axis, newPaneId, splitId),
    focused: newPaneId,
    maximised: null,
  };
}

/** Closes a pane; null when it was the last. */
export function closeInState(state: PaneState, id: string): PaneState | null {
  const tree = closePane(state.tree, id);
  if (tree === null) return null;
  if (tree === state.tree) return state;
  const focused = state.focused === id ? (focusAfterClose(state.tree, id) ?? leaves(tree)[0]) : state.focused;
  return { tree, focused, maximised: state.maximised === id ? null : state.maximised };
}

export function focusPane(state: PaneState, id: string): PaneState {
  if (id === state.focused || !leaves(state.tree).includes(id)) return state;
  // Focusing another pane while one is maximised moves the maximised view along.
  return { ...state, focused: id, maximised: state.maximised === null ? null : id };
}

export function moveFocus(state: PaneState, direction: Direction): PaneState {
  if (state.maximised !== null) return state;
  return focusPane(state, focusDirection(state.tree, state.focused, direction));
}

export function cycleFocus(state: PaneState, step: 1 | -1): PaneState {
  return focusPane(state, cyclePane(state.tree, state.focused, step));
}

export function toggleMaximise(state: PaneState): PaneState {
  if (leaves(state.tree).length < 2) return state;
  return { ...state, maximised: state.maximised === null ? state.focused : null };
}

/** Where each pane sits; with a maximised pane, only that one is visible. */
export function visibleLayout(state: PaneState): Layout {
  const full = layout(state.tree);
  if (state.maximised === null || !full.panes[state.maximised]) return full;
  return { panes: { [state.maximised]: { x: 0, y: 0, w: 1, h: 1 } }, dividers: [] };
}
