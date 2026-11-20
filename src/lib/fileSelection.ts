/**
 * The file pane's multi-selection: click selects one, Ctrl/⌘+click toggles,
 * Shift+click selects the range from the anchor, Ctrl/⌘+A selects all.
 * Pure functions over the listing order, so they are tested without a window.
 */

export interface Selection {
  /** Selected names, in listing order. */
  names: string[];
  /** Where a Shift range starts: the last entry clicked without Shift. */
  anchor: string | null;
}

export const emptySelection: Selection = { names: [], anchor: null };

export interface ClickModifiers {
  /** Ctrl on Windows and Linux, ⌘ on macOS. */
  toggle: boolean;
  shift: boolean;
}

/** `names` reordered to follow `order`; names not in `order` are dropped. */
function inOrder(order: string[], names: Iterable<string>): string[] {
  const wanted = new Set(names);
  return order.filter((name) => wanted.has(name));
}

export function selectOnly(name: string): Selection {
  return { names: [name], anchor: name };
}

/** The entries from `from` to `to`, whichever comes first. */
function between(order: string[], from: string, to: string): string[] {
  const a = order.indexOf(from);
  const b = order.indexOf(to);
  if (a < 0 || b < 0) return [to];
  return order.slice(Math.min(a, b), Math.max(a, b) + 1);
}

/** The selection after a click on `name` with the given modifiers. */
export function clickSelect(selection: Selection, order: string[], name: string, modifiers: ClickModifiers): Selection {
  if (!order.includes(name)) return selection;
  if (modifiers.shift) {
    const anchor = selection.anchor && order.includes(selection.anchor) ? selection.anchor : name;
    const range = between(order, anchor, name);
    // Ctrl+Shift adds the range to what is already selected.
    const names = modifiers.toggle ? inOrder(order, [...selection.names, ...range]) : range;
    return { names, anchor };
  }
  if (modifiers.toggle) {
    const has = selection.names.includes(name);
    const names = has ? selection.names.filter((candidate) => candidate !== name) : inOrder(order, [...selection.names, name]);
    return { names, anchor: name };
  }
  return selectOnly(name);
}

export function selectAll(order: string[]): Selection {
  return { names: [...order], anchor: order[0] ?? null };
}

/** The entry an arrow key moves to from `from`, or null at either end. */
export function neighbour(order: string[], from: string, step: 1 | -1): string | null {
  const index = order.indexOf(from);
  if (index < 0) return null;
  return order[index + step] ?? null;
}

/**
 * What a row action applies to: the whole selection when the row is part of
 * a multi-selection, otherwise just the row.
 */
export function targetsOf(selection: Selection, name: string): string[] {
  return selection.names.length > 1 && selection.names.includes(name) ? selection.names : [name];
}
