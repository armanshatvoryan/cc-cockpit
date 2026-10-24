/* The app shell never scrolls: every scrolling surface (xterm, sidebars,
 * panels) scrolls inside itself. Yet the whole page was once seen shifted
 * left with a blank strip on the right, and it stayed that way until it fixed
 * itself — the document scrolled sideways, presumably a focus jump revealing
 * something that sticks out past the right edge. The cause is not yet known.
 *
 * `overflow: hidden` on html/body (styles.css) stops user scrolling, but not
 * the browser's own reveal-on-focus. So: snap any document scroll straight
 * back, and record what stuck out at that moment in localStorage
 * (`cc-page-scroll-diag`, last 5 events) so the next occurrence names it.
 */
const DIAG_KEY = "cc-page-scroll-diag";

function describe(el: Element): string {
  const cls = typeof el.className === "string" ? el.className.trim().split(/\s+/).slice(0, 3).join(".") : "";
  const r = el.getBoundingClientRect();
  return `${el.tagName.toLowerCase()}${cls ? "." + cls : ""} [${Math.round(r.left)}..${Math.round(r.right)}]`;
}

/** Outermost elements whose right edge passes the viewport (children of an
 *  offender are skipped — they only repeat it). */
function offenders(limit: number): string[] {
  const out: string[] = [];
  const walk = (el: Element) => {
    if (out.length >= limit) return;
    if (el.getBoundingClientRect().right > window.innerWidth + 1) {
      out.push(describe(el));
      return;
    }
    for (const c of Array.from(el.children)) walk(c);
  };
  walk(document.body);
  return out;
}

function record(): void {
  try {
    const entry = {
      at: new Date().toISOString(),
      scrollX: window.scrollX,
      scrollY: window.scrollY,
      viewport: `${window.innerWidth}x${window.innerHeight}`,
      scrollWidth: document.documentElement.scrollWidth,
      focused: document.activeElement ? describe(document.activeElement) : null,
      offenders: offenders(8),
    };
    const prev = JSON.parse(localStorage.getItem(DIAG_KEY) ?? "[]");
    localStorage.setItem(DIAG_KEY, JSON.stringify([...prev, entry].slice(-5)));
  } catch {
    /* storage unavailable — the snap-back below still runs */
  }
}

export function installPageScrollGuard(): void {
  window.addEventListener(
    "scroll",
    () => {
      if (window.scrollX === 0 && window.scrollY === 0) return;
      record();
      window.scrollTo(0, 0);
    },
    { passive: true },
  );
}
