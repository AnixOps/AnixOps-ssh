import { Check, Copy, CornerDownLeft } from "lucide-react";
import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { copyText } from "@/lib/backend";
import { insertableCommand } from "@/lib/ai/commands";
import { parseMarkdown, type Block, type Inline } from "@/lib/ai/markdown";
import { t } from "@/lib/i18n";

/** An assistant answer as Markdown. Everything is rendered as React nodes
 *  (no HTML), links are shown but not followed, and each single-line code
 *  block gets an "Insert command" button that only fills the input line. */
export function AiMarkdown({
  text,
  onInsert,
}: {
  text: string;
  /** Puts a command on the terminal's input line; absent when it cannot. */
  onInsert?: (command: string) => void;
}) {
  const blocks = parseMarkdown(text);
  return (
    <div className="selectable space-y-2 text-callout font-normal text-label-1">
      {blocks.map((block, index) => (
        <BlockView key={index} block={block} onInsert={onInsert} />
      ))}
    </div>
  );
}

function BlockView({ block, onInsert }: { block: Block; onInsert?: (command: string) => void }) {
  switch (block.t) {
    case "p":
      return <p className="break-words">{renderInline(block.inline)}</p>;
    case "h":
      return <p className="text-callout font-semibold break-words">{renderInline(block.inline)}</p>;
    case "ul":
      return (
        <ul className="list-disc space-y-1 pl-5">
          {block.items.map((item, index) => (
            <li key={index}>{renderInline(item)}</li>
          ))}
        </ul>
      );
    case "ol":
      return (
        <ol className="list-decimal space-y-1 pl-5">
          {block.items.map((item, index) => (
            <li key={index}>{renderInline(item)}</li>
          ))}
        </ol>
      );
    case "quote":
      return (
        <blockquote className="border-l-2 border-separator pl-3 text-label-2">{renderInline(block.inline)}</blockquote>
      );
    case "code":
      return <CodeBlock code={block.code} lang={block.lang} open={block.open} onInsert={onInsert} />;
  }
}

function renderInline(parts: Inline[]): ReactNode {
  return parts.map((part, index) => {
    switch (part.t) {
      case "text":
        return <Fragment key={index}>{part.v}</Fragment>;
      case "code":
        return (
          <code key={index} className="rounded-xs bg-fill-1 px-1 font-mono text-callout">
            {part.v}
          </code>
        );
      case "strong":
        return (
          <strong key={index} className="font-semibold">
            {renderInline(part.v)}
          </strong>
        );
      case "em":
        return <em key={index}>{renderInline(part.v)}</em>;
      case "link":
        return (
          <span key={index} title={part.href} className="underline decoration-separator underline-offset-2">
            {part.text}
            <span className="text-label-3"> ({part.href})</span>
          </span>
        );
    }
  });
}

function CodeBlock({
  code,
  lang,
  open,
  onInsert,
}: {
  code: string;
  lang: string;
  /** The answer is still arriving, so the block may be incomplete. */
  open: boolean;
  onInsert?: (command: string) => void;
}) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const command = open ? null : insertableCommand(code);
  const copy = () => {
    void copyText(code).then(() => {
      setCopied(true);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setCopied(false), 1500);
    });
  };
  return (
    <div className="overflow-hidden rounded-sm bg-fill-1 ring-1 ring-separator">
      <div className="flex h-7 items-center gap-1 px-2">
        <span className="flex-1 truncate font-mono text-callout text-label-3">{lang}</span>
        <Tooltip label={t.askAiCopy}>
          <Button variant="quiet" size="icon-sm" aria-label={t.askAiCopy} onClick={copy}>
            {copied ? <Check /> : <Copy />}
          </Button>
        </Tooltip>
      </div>
      <pre className="selectable overflow-x-auto px-3 pb-2 font-mono text-callout whitespace-pre text-label-1">
        {code}
      </pre>
      {onInsert && !open && (
        <div className="border-t border-separator p-1.5">
          <Tooltip label={command ? t.askAiInsertTip : t.askAiInsertNo}>
            {/* A span keeps the tooltip working on a disabled button. */}
            <span className="inline-flex">
              <Button
                variant="secondary"
                size="sm"
                disabled={command === null}
                aria-label={t.askAiInsert}
                onClick={() => command && onInsert(command)}
              >
                <CornerDownLeft />
                {t.askAiInsert}
              </Button>
            </span>
          </Tooltip>
        </div>
      )}
    </div>
  );
}
