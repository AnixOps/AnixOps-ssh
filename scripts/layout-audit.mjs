// Layout assertions run on every screenshot scene (scripts/screenshots.mjs):
// finds text squeezed to a character or two per line, text that overlaps other
// text, text cut off by an `overflow: hidden` box without an ellipsis, and a
// page that scrolls sideways. Each finding names the text so the log says
// where to look. See docs/layout-checks.md.
//
// Usage: `const problems = await auditLayout(page)`; an empty array is a pass.
// An element (or an ancestor) with `data-layout-ignore` is skipped, for content
// that is meant to be clipped or stacked (a terminal, a chart, a drag ghost).

/** Runs in the page. Kept free of closures so Playwright can serialise it. */
function audit() {
  const problems = [];
  const describe = (element, text) => {
    const name = (text ?? element.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 40);
    const where = element.closest("[aria-label],[role=dialog],section,aside,main,header,nav");
    const label = where?.getAttribute("aria-label") || where?.querySelector("h1,h2,h3")?.textContent?.trim() || where?.tagName.toLowerCase() || "";
    return `"${name}"${label ? ` in ${label.slice(0, 30)}` : ""}`;
  };
  // `inert` content sits under a page that covers it (the sessions under Settings).
  const ignored = (element) => Boolean(element.closest("[data-layout-ignore], [inert], .xterm, [aria-hidden=true], svg, canvas"));
  const visible = (element) => {
    const style = getComputedStyle(element);
    if (style.visibility === "hidden" || style.display === "none" || Number(style.opacity) === 0) return false;
    return element.getClientRects().length > 0;
  };

  // The box an element can actually paint in: its viewport rect cut by every
  // ancestor that clips (overflow other than visible).
  const clipOf = (element) => {
    let box = { left: 0, top: 0, right: innerWidth, bottom: innerHeight };
    for (let node = element; node; node = node.parentElement) {
      const style = getComputedStyle(node);
      if (style.overflowX !== "visible" || style.overflowY !== "visible") {
        const rect = node.getBoundingClientRect();
        if (style.overflowX !== "visible") box = { ...box, left: Math.max(box.left, rect.left), right: Math.min(box.right, rect.right) };
        if (style.overflowY !== "visible") box = { ...box, top: Math.max(box.top, rect.top), bottom: Math.min(box.bottom, rect.bottom) };
      }
      if (style.position === "fixed") break;
    }
    return box;
  };

  // Text under a dialog, menu or toast is covered on purpose: whatever is on
  // top at its centre sits in a fixed layer that does not contain it.
  const coveredByLayer = (element, box, clip) => {
    const x = (Math.max(box.left, clip.left) + Math.min(box.right, clip.right)) / 2;
    const y = (Math.max(box.top, clip.top) + Math.min(box.bottom, clip.bottom)) / 2;
    const top = document.elementFromPoint(x, y);
    if (!top || element.contains(top) || top.contains(element)) return false;
    for (let node = top; node && node !== document.body; node = node.parentElement) {
      if (node.contains(element)) return false;
      if (/fixed|sticky/.test(getComputedStyle(node).position) || node.matches("[role=dialog],[role=menu],[role=listbox],[role=tooltip]")) return true;
    }
    return false;
  };

  const layerOver = (top, other) => {
    for (let node = top; node && node !== document.body; node = node.parentElement) {
      if (node.contains(other)) return false;
      const position = getComputedStyle(node).position;
      if (position === "fixed" || position === "sticky" || node.matches("[role=dialog],[role=menu],[role=listbox],[role=tooltip]")) return true;
    }
    return false;
  };

  // Every visible run of text, measured line by line with a Range.
  const runs = [];
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const text = node.textContent.replace(/\s+/g, " ").trim();
    const element = node.parentElement;
    if (!text || !element || ignored(element) || !visible(element)) continue;
    // Screen-reader-only text is clipped to a pixel on purpose (a box that
    // is squeezed to nothing in one direction only is the bug we look for).
    const own = element.getBoundingClientRect();
    if (own.width <= 2 && own.height <= 2) continue;
    const range = document.createRange();
    range.selectNodeContents(node);
    const lines = [...range.getClientRects()].filter((rect) => rect.width > 0.5 && rect.height > 0.5);
    if (lines.length === 0) continue;
    const style = getComputedStyle(element);
    // A truncated label squeezed below two characters shows only "…".
    const font = parseFloat(style.fontSize) || 13;
    if (style.textOverflow === "ellipsis" && text.length >= 3 && own.width < font * 2.6 && own.height > 2 && element.scrollWidth > element.clientWidth + 1)
      problems.push(`truncated to ${Math.round(own.width)} px: ${describe(element, text)}`);
    const clip = clipOf(element);
    const box = range.getBoundingClientRect();
    if (box.bottom <= clip.top || box.top >= clip.bottom || box.right <= clip.left || box.left >= clip.right) continue; // scrolled away
    runs.push({ node, element, text, lines, box, clip, style, covered: coveredByLayer(element, box, clip) });
  }

  for (const run of runs) {
    const fontSize = parseFloat(run.style.fontSize) || 13;
    // 1. Squeezed: three or more lines, none wider than about two characters.
    const widest = Math.max(...run.lines.map((rect) => rect.width));
    const rows = new Set(run.lines.map((rect) => Math.round(rect.top))).size;
    if (rows >= 3 && run.text.length >= 4 && widest < fontSize * 2.6)
      problems.push(`squeezed to ${rows} lines of ${Math.round(widest)} px: ${describe(run.element, run.text)}`);

    // 2. Cut off sideways by a clipping box with no ellipsis to say so.
    const cut = run.box.right - run.clip.right > 1.5 || run.clip.left - run.box.left > 1.5;
    if (cut) {
      let ellipsis = false;
      let scrolls = false;
      for (let node = run.element; node && node !== document.body; node = node.parentElement) {
        const style = getComputedStyle(node);
        if (style.textOverflow === "ellipsis") ellipsis = true;
        if (style.overflowX === "auto" || style.overflowX === "scroll") scrolls = true;
        if (style.overflowX !== "visible") break;
      }
      // Inputs and selects scroll their own text; a horizontal scroller is fine.
      if (!ellipsis && !scrolls && !run.element.closest("input,textarea,select,option"))
        problems.push(`cut off by ${Math.round(Math.max(run.box.right - run.clip.right, run.clip.left - run.box.left))} px without an ellipsis: ${describe(run.element, run.text)}`);
    }
  }

  // 3. Text over text: two runs whose painted lines intersect, unless one is
  // inside the other's element (a badge in a label) or a layer covers both.
  const area = (a, b) => Math.max(0, Math.min(a.right, b.right) - Math.max(a.left, b.left)) * Math.max(0, Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top));
  const clipped = (rect, clip) => ({ left: Math.max(rect.left, clip.left), right: Math.min(rect.right, clip.right), top: Math.max(rect.top, clip.top), bottom: Math.min(rect.bottom, clip.bottom) });
  const seen = new Set();
  for (let i = 0; i < runs.length; i++) {
    for (let j = i + 1; j < runs.length; j++) {
      const a = runs[i];
      const b = runs[j];
      if (a.covered || b.covered) continue;
      if (a.element === b.element || a.element.contains(b.element) || b.element.contains(a.element)) continue;
      if (area(a.box, b.box) < 4) continue;
      for (const la of a.lines) {
        const ra = clipped(la, a.clip);
        if (ra.right - ra.left < 1 || ra.bottom - ra.top < 1) continue; // scrolled out of its box
        const hit = b.lines
          .map((lb) => clipped(lb, b.clip))
          .filter((rb) => rb.right - rb.left >= 1 && rb.bottom - rb.top >= 1)
          .find((rb) => area(ra, rb) > Math.min(12, 0.15 * (ra.bottom - ra.top) * (ra.right - ra.left)));
        if (!hit) continue;
        // Whatever is on top at the overlap must be one of the two texts;
        // otherwise a dialog or menu covers both and the user sees neither.
        const x = (Math.max(ra.left, hit.left) + Math.min(ra.right, hit.right)) / 2;
        const y = (Math.max(ra.top, hit.top) + Math.min(ra.bottom, hit.bottom)) / 2;
        const top = document.elementFromPoint(x, y);
        if (!top || (!a.element.contains(top) && !b.element.contains(top) && !top.contains(a.element) && !top.contains(b.element))) continue;
        // One text sits in a layer over the other (a menu, a sticky header or
        // footer, a toast) that the other scrolls or lies under: intended.
        const under = a.element.contains(top) || top.contains(a.element) ? b.element : a.element;
        if (layerOver(top, under)) continue;
        const key = `${a.text}|${b.text}`;
        if (!seen.has(key)) {
          seen.add(key);
          problems.push(`text overlaps text: ${describe(a.element, a.text)} and ${describe(b.element, b.text)}`);
        }
        break;
      }
    }
  }

  // 4. The window itself never scrolls sideways.
  const root = document.scrollingElement;
  if (root && root.scrollWidth > root.clientWidth + 1) problems.push(`page scrolls sideways: ${root.scrollWidth} px wide in a ${root.clientWidth} px window`);
  return problems;
}

/** Scrolls every scroll area through its content (a page at a time) so text
 *  below the fold is checked too, then puts it back. */
function scrollers() {
  return [...document.querySelectorAll("*")]
    .filter((element) => {
      if (element.closest("[data-layout-ignore], .xterm")) return false;
      const style = getComputedStyle(element);
      return /auto|scroll/.test(style.overflowY) && element.scrollHeight > element.clientHeight + 4 && element.clientHeight > 40;
    })
    .map((element, index) => (element.setAttribute("data-layout-scroller", String(index)), { index, top: element.scrollTop, steps: Math.min(8, Math.ceil(element.scrollHeight / element.clientHeight)) }));
}

async function auditOnce(page) {
  const problems = new Set(await page.evaluate(audit));
  const areas = await page.evaluate(scrollers);
  for (const area of areas) {
    for (let step = 1; step <= area.steps; step++) {
      await page.evaluate(({ index, step }) => {
        const element = document.querySelector(`[data-layout-scroller="${index}"]`);
        if (element) element.scrollTop = step * element.clientHeight * 0.9;
      }, { index: area.index, step });
      for (const problem of await page.evaluate(audit)) problems.add(problem);
    }
    await page.evaluate(({ index, top }) => {
      const element = document.querySelector(`[data-layout-scroller="${index}"]`);
      if (element) (element.scrollTop = top), element.removeAttribute("data-layout-scroller");
    }, area);
  }
  return problems;
}

/** Audits twice, a moment apart, and reports what both runs found, so a
 *  frame caught mid-animation is not a failure. */
export async function auditLayout(page) {
  const first = await auditOnce(page);
  if (first.size === 0) return [];
  await page.waitForTimeout(600);
  const second = await auditOnce(page);
  return [...first].filter((problem) => second.has(problem));
}
