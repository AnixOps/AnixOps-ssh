/** Selection of rows in the file pane: click, Shift+click range, Ctrl/Cmd+click toggle, select all. */

export interface Selection {
  /** Names of the selected entries. */
  names: ReadonlySet<string>;
  /** The row a Shift range grows from. */
  anchor: string | null;
}

export const emptySelection: Selection = { names: new Set(), anchor: null };

export interface ClickModifiers {
  shift?: boolean;
  /** Ctrl on Windows and Linux, Cmd on macOS. */
  toggle?: boolean;
}

/**
 * The selection after a click on `name`, in the listing order `order`.
 * Plain click selects only that row; Ctrl/Cmd toggles it; Shift selects the
 * rows between the anchor and it (added to the selection with Ctrl/Cmd).
 */
export function clickSelect(current: Selection, order: readonly string[], name: string, mods: ClickModifiers = {}): Selection {
  if (mods.shift && current.anchor !== null && order.includes(current.anchor)) {
    const from = order.indexOf(current.anchor);
    const to = order.indexOf(name);
    if (to < 0) return current;
    const range = order.slice(Math.min(from, to), Math.max(from, to) + 1);
    const names = new Set(mods.toggle ? current.names : []);
    for (const item of range) names.add(item);
    return { names, anchor: current.anchor };
  }
  if (mods.toggle) {
    const names = new Set(current.names);
    if (names.has(name)) names.delete(name);
    else names.add(name);
    return { names, anchor: name };
  }
  return { names: new Set([name]), anchor: name };
}

export function selectAll(order: readonly string[]): Selection {
  return { names: new Set(order), anchor: order[0] ?? null };
}

/** Keeps only the selected names that are still listed (after a refresh). */
export function prune(current: Selection, order: readonly string[]): Selection {
  const listed = new Set(order);
  const names = new Set([...current.names].filter((name) => listed.has(name)));
  if (names.size === current.names.size) return current;
  return { names, anchor: current.anchor !== null && listed.has(current.anchor) ? current.anchor : null };
}

/** The selected names in listing order. */
export function inOrder(current: Selection, order: readonly string[]): string[] {
  return order.filter((name) => current.names.has(name));
}

/**
 * What an action on the row `name` applies to: the whole selection when the
 * row is part of it, otherwise just that row (as file managers do).
 */
export function actionTargets(current: Selection, order: readonly string[], name: string): string[] {
  return current.names.has(name) ? inOrder(current, order) : [name];
}
