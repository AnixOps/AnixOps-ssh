/* The window is a web view, but it should never act like a browser: no
   "Back / Reload / Inspect" menu on a right-click, no reload, print, find bar,
   page zoom or caret browsing from the keyboard, no history navigation from the
   mouse's side buttons, no dropped file replacing the page. The app's own
   menus (terminal, lists, text fields: components/ui/editMenu.tsx) take the
   right-click instead. On Windows the release build also turns the same things
   off in WebView2 itself (src-tauri/src/webview_chrome.rs).

   Development builds keep a way in for the inspector: Shift+right-click shows
   the web view's menu, and F5, F12 and Ctrl+Shift+I still work. */

import { isMac } from "@/lib/shortcuts";

export interface BrowserKeyInput {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/** Whether a key press would reach a browser feature (reload, print, find,
 *  save page, view source, zoom, caret browsing, history, the inspector), so
 *  its default must be stopped. Editing keys (copy, cut, paste, undo, select
 *  all in a text field) are never caught: `editable` says the focus is in one. */
export function isBrowserKey(
  event: BrowserKeyInput,
  { mac, dev, editable }: { mac: boolean; dev: boolean; editable: boolean },
): boolean {
  const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;
  const mod = mac ? event.metaKey : event.ctrlKey;
  if (key === "BrowserBack" || key === "BrowserForward" || key === "BrowserRefresh" || key === "BrowserHome" || key === "BrowserSearch")
    return true;
  // Reload, find next, caret browsing; F12 opens the inspector.
  if (key === "F5" || key === "F3" || key === "F7") return !(dev && key === "F5");
  if (key === "F12") return !dev;
  // Alt+Left and Alt+Right go back and forward (Windows and Linux). On a Mac,
  // Option+Arrow moves by word in a text field and has no browser meaning.
  if (!mac && event.altKey && !event.ctrlKey && !event.metaKey && (key === "ArrowLeft" || key === "ArrowRight")) return true;
  if (!mod) return false;
  // The inspector and its consoles.
  if (event.shiftKey && (key === "i" || key === "j" || key === "c" || key === "k")) return !dev && (mac ? event.altKey : true);
  if (mac && event.altKey && (key === "i" || key === "j" || key === "c" || key === "u")) return !dev;
  // Reload, print, save page, view source, find (bar, next, previous), open
  // file, history, downloads.
  if (key === "r") return !dev;
  if (key === "p" || key === "s" || key === "u" || key === "f" || key === "g" || key === "o" || key === "h" || key === "j") return true;
  // Page zoom.
  if (key === "+" || key === "=" || key === "-" || key === "_" || key === "0") return true;
  // Select all outside a text field would highlight the whole window.
  if (key === "a" && !event.shiftKey && !event.altKey) return !editable;
  return false;
}

/** A text field the user can type in: the right-click there gets the editing menu. */
export function editableTarget(target: EventTarget | null): HTMLInputElement | HTMLTextAreaElement | HTMLElement | null {
  if (!(target instanceof Element)) return null;
  const element = target.closest<HTMLElement>("input, textarea, [contenteditable]:not([contenteditable='false'])");
  if (!element) return null;
  if (element instanceof HTMLInputElement) {
    const textual = ["text", "search", "url", "tel", "email", "password", "number", ""].includes(element.type);
    return textual ? element : null;
  }
  return element;
}

/** Shift+right-click in a development build shows the web view's own menu (Inspect). */
export function nativeMenuAllowed(event: MouseEvent): boolean {
  return import.meta.env.DEV && event.shiftKey;
}

/** Install the guards on the window. Desktop only: on a phone the long press
 *  belongs to the system's own text selection. */
export function installWebviewGuards(win: Window = window): void {
  const dev = import.meta.env.DEV;
  const windows = /Win/.test(win.navigator.platform);

  // Last in line (window, bubbling): menus that handled the event already
  // stopped it; whatever is left must not show the web view's menu.
  win.addEventListener("contextmenu", (event) => {
    if (!nativeMenuAllowed(event)) event.preventDefault();
  });

  win.addEventListener("keydown", (event) => {
    if (event.defaultPrevented) return;
    if (isBrowserKey(event, { mac: isMac, dev, editable: editableTarget(event.target) !== null })) event.preventDefault();
  });

  // The mouse's back and forward buttons navigate on release.
  const sideButtons = (event: MouseEvent) => {
    if (event.button === 3 || event.button === 4) event.preventDefault();
  };
  win.addEventListener("mouseup", sideButtons);
  win.addEventListener("auxclick", sideButtons);
  // WebView2 starts auto-scrolling on a middle press; text fields keep the
  // middle button (Linux pastes the selection with it).
  if (windows)
    win.addEventListener("mousedown", (event) => {
      if (event.button === 1 && !editableTarget(event.target)) event.preventDefault();
    });

  // Ctrl+wheel and the trackpad pinch zoom the page.
  win.addEventListener(
    "wheel",
    (event) => {
      if (event.ctrlKey) event.preventDefault();
    },
    { passive: false },
  );

  // A file or link dropped where nothing takes it would replace the page.
  // Text dragged into a text field still lands there.
  win.addEventListener("dragover", (event) => {
    if (event.defaultPrevented || editableTarget(event.target)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "none";
  });
  win.addEventListener("drop", (event) => {
    if (!editableTarget(event.target)) event.preventDefault();
  });
}
