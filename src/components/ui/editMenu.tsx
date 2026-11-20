import { ClipboardPaste, Copy, Scissors, TextSelect, Undo2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { ContextMenu, type MenuEntry } from "@/components/ui/menu";
import { copyText, pasteText } from "@/lib/backend";
import { t } from "@/lib/i18n";
import { editableTarget, nativeMenuAllowed } from "@/lib/webviewChrome";

type Field = HTMLInputElement | HTMLTextAreaElement;

/** What was under the pointer: a text field (and its selection at the time),
 *  or selected text elsewhere on the page. */
type Target =
  | { kind: "field"; element: HTMLElement; start: number; end: number; range: Range | null }
  | { kind: "selection"; text: string };

const isField = (element: HTMLElement): element is Field =>
  element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement;

function selectedText(target: Extract<Target, { kind: "field" }>): string {
  if (isField(target.element)) return target.element.value.slice(target.start, target.end);
  return target.range?.toString() ?? "";
}

/** Put the focus and the selection back where they were when the menu opened. */
function restore(target: Extract<Target, { kind: "field" }>) {
  const { element } = target;
  element.focus();
  if (isField(element)) {
    try {
      element.setSelectionRange(target.start, target.end);
    } catch {
      // Number fields have no selection.
    }
  } else if (target.range) {
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(target.range);
  }
}

/** Replace the selection with `text`, through the editor so that undo and the
 *  field's change handlers see it. */
function insert(element: HTMLElement, text: string) {
  if (document.execCommand("insertText", false, text)) return;
  if (!isField(element)) return;
  element.setRangeText(text, element.selectionStart ?? 0, element.selectionEnd ?? 0, "end");
  element.dispatchEvent(new Event("input", { bubbles: true }));
}

/** The app's right-click menu for text: Undo, Cut, Copy, Paste and Select all
 *  in a text field, Copy on selected text anywhere else. Everything else gets
 *  no menu (lib/webviewChrome.ts keeps the web view's own one away). Places
 *  with a menu of their own (terminal, lists, tabs) handle the event first. */
export function EditContextMenu() {
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  const [target, setTarget] = useState<Target | null>(null);
  const targetRef = useRef<Target | null>(null);

  useEffect(() => {
    const open = (event: MouseEvent) => {
      if (event.defaultPrevented || nativeMenuAllowed(event)) return;
      const element = editableTarget(event.target);
      if (element) {
        const disabled = (element as Field).disabled === true;
        if (disabled) return;
        let start = 0;
        let end = 0;
        let range: Range | null = null;
        if (isField(element)) {
          start = element.selectionStart ?? 0;
          end = element.selectionEnd ?? 0;
        } else {
          const selection = window.getSelection();
          if (selection && selection.rangeCount > 0 && element.contains(selection.anchorNode)) range = selection.getRangeAt(0).cloneRange();
        }
        targetRef.current = { kind: "field", element, start, end, range };
      } else {
        const text = window.getSelection()?.toString() ?? "";
        if (!text.trim()) return;
        targetRef.current = { kind: "selection", text };
      }
      event.preventDefault();
      setTarget(targetRef.current);
      setAt({ x: event.clientX, y: event.clientY });
    };
    document.addEventListener("contextmenu", open);
    return () => document.removeEventListener("contextmenu", open);
  }, []);

  // The focus goes back to the field (a chosen item then restores the selection).
  const close = useCallback(() => {
    setAt(null);
    const current = targetRef.current;
    if (current?.kind === "field") current.element.focus();
  }, []);

  let items: MenuEntry[] = [];
  if (target?.kind === "selection") {
    items = [{ key: "copy", label: t.copy, icon: <Copy />, onSelect: () => void copyText(target.text).catch(console.error) }];
  } else if (target?.kind === "field") {
    const { element } = target;
    const writable = isField(element) ? !element.readOnly : element.isContentEditable;
    const secret = element instanceof HTMLInputElement && element.type === "password";
    const text = selectedText(target);
    items = [
      {
        key: "undo",
        label: t.undo,
        icon: <Undo2 />,
        disabled: !writable,
        onSelect: () => {
          restore(target);
          document.execCommand("undo");
        },
      },
      "separator",
      {
        key: "cut",
        label: t.cut,
        icon: <Scissors />,
        disabled: !writable || secret || !text,
        onSelect: () => {
          restore(target);
          void copyText(text)
            .then(() => insert(element, ""))
            .catch(console.error);
        },
      },
      {
        key: "copy",
        label: t.copy,
        icon: <Copy />,
        disabled: secret || !text,
        onSelect: () => {
          restore(target);
          void copyText(text).catch(console.error);
        },
      },
      {
        key: "paste",
        label: t.paste,
        icon: <ClipboardPaste />,
        disabled: !writable,
        onSelect: () => {
          restore(target);
          void pasteText()
            .then((pasted) => {
              if (!pasted) return;
              restore(target);
              insert(element, pasted);
            })
            .catch(console.error);
        },
      },
      "separator",
      {
        key: "select-all",
        label: t.selectAll,
        icon: <TextSelect />,
        onSelect: () => {
          element.focus();
          if (isField(element)) element.select();
          else document.execCommand("selectAll");
        },
      },
    ];
  }

  return (
    <ContextMenu label={target?.kind === "selection" ? t.selectionMenu : t.editMenu} at={at} items={items} onClose={close} />
  );
}
