/**
 * The in-page snapshot collector.
 *
 * Shipped as a SOURCE STRING and evaluated in the page: the walker never
 * sends closures (a serialized closure that touches module scope is a
 * ReferenceError inside the browser, invisible to the compiler).
 *
 * Guarantees the rest of the loop leans on:
 * - every emitted selector is UNIQUE on the page and printable ASCII — the
 *   capability surface rejects anything else (RF4007, pattern ^[ -~]+$), and
 *   a non-unique path silently resolves to the wrong element at replay;
 * - selectors prefer stability: #id → data-attributes → href → rooted
 *   nth-of-type path (walked to a unique #id or the document root — a
 *   depth-capped path is RELATIVE and matches wherever the shape repeats);
 * - besides interactive elements it captures short own-text leaves, because
 *   on React sites clickable tabs are plain `<div onClick>` invisible to any
 *   role/attribute scan, and labels/values are assertion targets.
 */
export const SNAPSHOT_SOURCE = `(withGeo) => {
  const ascii = (s) => /^[\\x20-\\x7e]+$/.test(s);
  const uniq = (s) => { try { return ascii(s) && document.querySelectorAll(s).length === 1; } catch (e) { return false; } };
  const esc = (s) => (window.CSS && CSS.escape) ? CSS.escape(s) : s.replace(/[^a-zA-Z0-9_-]/g, "\\\\$&");
  const attrSel = (tag, a, v) => tag + "[" + a + '="' + String(v).replace(/"/g, '\\\\"') + '"]';
  const lastSeg = (href) => {
    try { const u = new URL(href, location.href); const parts = u.pathname.split("/").filter(Boolean); return parts.slice(-2).join("/"); }
    catch (e) { return ""; }
  };
  const nthPath = (el) => {
    const parts = [];
    let cur = el;
    while (cur && cur.nodeType === 1) {
      if (cur.id && uniq("#" + esc(cur.id))) { parts.unshift("#" + esc(cur.id)); break; }
      const tag = cur.tagName.toLowerCase();
      let n = 1, sib = cur;
      while ((sib = sib.previousElementSibling)) if (sib.tagName === cur.tagName) n++;
      parts.unshift(tag + ":nth-of-type(" + n + ")");
      cur = cur.parentElement;
    }
    const sel = parts.join(" > ");
    return uniq(sel) ? sel : "";
  };
  const selectorFor = (el) => {
    const tag = el.tagName.toLowerCase();
    if (el.id && uniq("#" + esc(el.id))) return "#" + esc(el.id);
    for (const a of ["data-testid", "data-e2e", "data-bn-type", "data-testkey", "name", "aria-label", "placeholder", "type", "role"]) {
      const v = el.getAttribute(a);
      if (v && v.length < 60) { const s = attrSel(tag, a, v); if (uniq(s)) return s; }
    }
    const href = el.getAttribute("href");
    if (href) {
      const seg = lastSeg(href);
      if (seg) { const s = tag + '[href*="' + seg.replace(/"/g, '') + '"]'; if (uniq(s)) return s; }
      const s2 = attrSel(tag, "href", href); if (uniq(s2)) return s2;
    }
    return nthPath(el);
  };
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return false;
    const st = getComputedStyle(el);
    return st.visibility !== "hidden" && st.display !== "none" && Number(st.opacity) > 0.05;
  };
  const trim = (s) => (s || "").replace(/\\s+/g, " ").trim().slice(0, 70);
  const ownText = (el) => { let t = ""; el.childNodes.forEach((n) => { if (n.nodeType === 3) t += n.textContent; }); return t.replace(/\\s+/g, " ").trim(); };

  const interactiveSel = 'a,button,input,select,textarea,[role="button"],[role="tab"],[role="link"],[role="menuitem"],[role="option"],[tabindex],[onclick],[contenteditable="true"]';
  const CAP = 520;
  const set = new Set();
  const out = [];
  const push = (el, kind, textOverride) => {
    if (!el || set.has(el) || !visible(el) || out.length >= CAP) return;
    set.add(el);
    const sel = selectorFor(el);
    if (!sel) return;
    const rec = { sel, tag: el.tagName.toLowerCase(), kind };
    if (withGeo) {
      const r = el.getBoundingClientRect();
      rec.b = [Math.round(r.x + window.scrollX), Math.round(r.y + window.scrollY), Math.round(r.width), Math.round(r.height)];
    }
    const role = el.getAttribute("role"); if (role) rec.role = role;
    const t = trim(textOverride != null ? textOverride : (el.value || el.getAttribute("aria-label") || el.getAttribute("placeholder") || ownText(el)));
    if (t) rec.text = t;
    const ph = el.getAttribute("placeholder"); if (ph) rec.placeholder = trim(ph);
    const href = el.getAttribute("href"); if (href) rec.href = href.slice(0, 80);
    const ty = el.getAttribute("type"); if (ty) rec.type = ty;
    out.push(rec);
  };
  document.querySelectorAll(interactiveSel).forEach((el) => push(el, "interactive"));
  document.querySelectorAll("h1,h2,h3,h4,[role='heading'],label,th,td,span,div,li,p").forEach((el) => {
    const own = ownText(el);
    if (!own || own.length > 40) return;
    if (el.querySelector(interactiveSel)) return;
    if (el.children.length > 4) return;
    push(el, "text", own);
  });
  return { url: location.href, title: document.title, elements: out.slice(0, CAP) };
}`;

import type { WalkPageLike, WalkSnapshot } from "./types.ts";

/** Takes one snapshot of the page's current state. */
export async function takeSnapshot(page: WalkPageLike, geometry: boolean): Promise<WalkSnapshot> {
  const raw = await page.evaluate(`(${SNAPSHOT_SOURCE})(${geometry ? "true" : "false"})`);
  const snapshot = raw as WalkSnapshot;
  if (!snapshot || !Array.isArray(snapshot.elements)) {
    throw new Error("page snapshot did not return the expected shape");
  }
  return snapshot;
}
