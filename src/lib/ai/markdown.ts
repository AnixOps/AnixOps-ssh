/** A small Markdown reader for assistant answers: paragraphs, headings,
 *  lists, quotes, fenced code, and inline code, bold, italic and links. It
 *  produces data, never HTML, and tolerates text that is still arriving (an
 *  unclosed fence is an open code block). Links are shown, not followed. */

export type Inline =
  | { t: "text"; v: string }
  | { t: "code"; v: string }
  | { t: "strong"; v: Inline[] }
  | { t: "em"; v: Inline[] }
  | { t: "link"; text: string; href: string };

export type Block =
  | { t: "p"; inline: Inline[] }
  | { t: "h"; level: number; inline: Inline[] }
  | { t: "code"; lang: string; code: string; open: boolean }
  | { t: "ul" | "ol"; items: Inline[][] }
  | { t: "quote"; inline: Inline[] };

const FENCE = /^\s{0,3}(`{3,}|~{3,})\s*([\w+#.-]*)\s*$/;
const HEADING = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/;
const BULLET = /^\s*[-*+]\s+(.*)$/;
const NUMBERED = /^\s*\d+[.)]\s+(.*)$/;
const QUOTE = /^\s{0,3}>\s?(.*)$/;

export function parseMarkdown(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  const flush = () => {
    if (paragraph.length > 0) blocks.push({ t: "p", inline: parseInline(paragraph.join(" ")) });
    paragraph = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = FENCE.exec(line);
    if (fence) {
      flush();
      const marker = fence[1];
      const body: string[] = [];
      let closed = false;
      for (i++; i < lines.length; i++) {
        const close = lines[i].trim();
        if (close.length >= marker.length && close === marker[0].repeat(close.length)) {
          closed = true;
          break;
        }
        body.push(lines[i]);
      }
      blocks.push({ t: "code", lang: fence[2], code: body.join("\n"), open: !closed });
      continue;
    }
    if (line.trim() === "") {
      flush();
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      flush();
      blocks.push({ t: "h", level: heading[1].length, inline: parseInline(heading[2]) });
      continue;
    }
    const bullet = BULLET.exec(line);
    const numbered = bullet ? null : NUMBERED.exec(line);
    if (bullet || numbered) {
      flush();
      const kind = bullet ? "ul" : "ol";
      const pattern = bullet ? BULLET : NUMBERED;
      const items: Inline[][] = [];
      for (; i < lines.length; i++) {
        const item = pattern.exec(lines[i]);
        if (!item) break;
        items.push(parseInline(item[1]));
      }
      i--;
      blocks.push({ t: kind, items });
      continue;
    }
    const quote = QUOTE.exec(line);
    if (quote) {
      flush();
      const text: string[] = [];
      for (; i < lines.length; i++) {
        const q = QUOTE.exec(lines[i]);
        if (!q) break;
        text.push(q[1]);
      }
      i--;
      blocks.push({ t: "quote", inline: parseInline(text.join(" ")) });
      continue;
    }
    paragraph.push(line.trim());
  }
  flush();
  return blocks;
}

export function parseInline(source: string): Inline[] {
  const out: Inline[] = [];
  let text = "";
  const pushText = () => {
    if (text) out.push({ t: "text", v: text });
    text = "";
  };
  let i = 0;
  while (i < source.length) {
    const c = source[i];
    if (c === "\\" && i + 1 < source.length && /[\\`*_[\]()#>-]/.test(source[i + 1])) {
      text += source[i + 1];
      i += 2;
      continue;
    }
    if (c === "`") {
      let run = 1;
      while (source[i + run] === "`") run++;
      const ticks = "`".repeat(run);
      const end = source.indexOf(ticks, i + run);
      if (end > i + run) {
        pushText();
        out.push({ t: "code", v: source.slice(i + run, end).replace(/^ (.*) $/, "$1") });
        i = end + run;
        continue;
      }
      text += ticks;
      i += run;
      continue;
    }
    if (c === "*" || c === "_") {
      const double = source[i + 1] === c;
      const mark = double ? c + c : c;
      const start = i + mark.length;
      const end = source.indexOf(mark, start);
      const inner = end > start ? source.slice(start, end) : "";
      // `snake_case_names` are words, not emphasis; the content must not
      // start or end with a space.
      const intraword = c === "_" && /\w/.test(source[i - 1] ?? " ");
      if (end > start && inner.trim() === inner && !intraword) {
        pushText();
        out.push({ t: double ? "strong" : "em", v: parseInline(inner) });
        i = end + mark.length;
        continue;
      }
    }
    if (c === "[") {
      const close = source.indexOf("](", i + 1);
      const end = close > 0 ? source.indexOf(")", close + 2) : -1;
      if (close > i + 1 && end > close) {
        pushText();
        out.push({ t: "link", text: source.slice(i + 1, close), href: source.slice(close + 2, end) });
        i = end + 1;
        continue;
      }
    }
    text += c;
    i++;
  }
  pushText();
  return out;
}
