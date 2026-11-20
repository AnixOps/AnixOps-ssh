import { useEffect, useState, type RefObject } from "react";

/** The narrowest a terminal gets beside the side panes: about 56 columns at
 *  the default font size, enough for a prompt and a command. */
export const TERMINAL_MIN_WIDTH = 440;
/** Width of the folded monitor (MonitorPane's rail). */
export const MONITOR_RAIL_WIDTH = 52;
/** Width of the files pane (`w-100`). */
export const FILES_WIDTH = 400;

export interface WorkspaceFit {
  /** The monitor shows as its rail although the user left it open. */
  monitorRail: boolean;
  /** The files pane floats over the terminal instead of taking width. */
  filesOverlay: boolean;
}

/** Decides how the side panes of a tab fit in `width` px so the terminal
 *  keeps at least TERMINAL_MIN_WIDTH: first the monitor folds to its rail,
 *  then the files pane floats over the terminal. Nothing is remembered; a
 *  wider window gets the panes back as the user left them. */
export function fitWorkspace({
  width,
  monitor,
  filesOpen,
}: {
  width: number;
  monitor: { open: boolean; collapsed: boolean; width: number };
  filesOpen: boolean;
}): WorkspaceFit {
  const fit = { monitorRail: false, filesOverlay: false };
  // Not measured yet (a hidden tab): keep the user's layout.
  if (width <= 0) return fit;
  const files = filesOpen ? FILES_WIDTH : 0;
  let side = monitor.open ? (monitor.collapsed ? MONITOR_RAIL_WIDTH : monitor.width) : 0;
  if (width - side - files >= TERMINAL_MIN_WIDTH) return fit;
  if (monitor.open && !monitor.collapsed) {
    fit.monitorRail = true;
    side = MONITOR_RAIL_WIDTH;
  }
  if (filesOpen && width - side - files < TERMINAL_MIN_WIDTH) fit.filesOverlay = true;
  return fit;
}

/** The content width of an element, kept current with a ResizeObserver. */
export function useElementWidth(ref: RefObject<HTMLElement | null>): number {
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    setWidth(element.clientWidth);
    const observer = new ResizeObserver(() => setWidth(element.clientWidth));
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}
