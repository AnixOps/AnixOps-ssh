import { createElement, isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { Button } from "@/components/ui/button";
import { FirstRunCard } from "./FirstRunCard";

const strings = { title: "Help fix crashes?", body: "Never contains hosts.", allow: "Send", deny: "Do not send" };

function buttons(node: ReactNode, found: ReactElement<Record<string, unknown>>[] = []) {
  if (Array.isArray(node)) node.forEach((child) => buttons(child, found));
  else if (isValidElement<Record<string, unknown>>(node)) {
    if (node.type === Button) found.push(node);
    buttons(node.props.children as ReactNode, found);
  }
  return found;
}

describe("first-run card", () => {
  it("offers two buttons that look the same and carry no pre-selection", () => {
    const tree = FirstRunCard({ strings, onAnswer: () => {} });
    const [allow, deny] = buttons(tree);
    expect(buttons(tree)).toHaveLength(2);
    for (const key of ["variant", "size", "className", "autoFocus", "disabled", "aria-pressed", "aria-checked"]) {
      expect(allow.props[key], key).toEqual(deny.props[key]);
    }
    expect(allow.props.variant).toBe("secondary");
    expect(allow.props.autoFocus).toBeUndefined();
  });

  it("answers yes and no, once per click", () => {
    const onAnswer = vi.fn();
    const [allow, deny] = buttons(FirstRunCard({ strings, onAnswer }));
    (deny.props.onClick as () => void)();
    (allow.props.onClick as () => void)();
    expect(onAnswer.mock.calls).toEqual([[false], [true]]);
  });

  it("is a labelled region, not a modal dialog, and shows the strings", () => {
    const html = renderToStaticMarkup(createElement(FirstRunCard, { strings, onAnswer: () => {} }));
    expect(html).toContain('role="region"');
    expect(html).toContain('aria-labelledby="first-run-title"');
    expect(html).not.toContain("aria-modal");
    expect(html).not.toContain('role="dialog"');
    expect(html).not.toContain("autofocus");
    for (const text of [strings.title, strings.body, strings.allow, strings.deny]) expect(html).toContain(text);
  });

  it("uses design tokens only", () => {
    const html = renderToStaticMarkup(createElement(FirstRunCard, { strings, onAnswer: () => {} }));
    expect(html).not.toMatch(/#[0-9a-fA-F]{3,8}\b|rgb\(|\[#/);
  });
});
