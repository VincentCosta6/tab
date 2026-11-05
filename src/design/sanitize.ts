// Cleans untrusted HTML (email bodies) before it is shown.
//
// This is one of three layers. The frame that shows the result also has no
// `allow-scripts` in its sandbox (nothing can run even if something slips
// through) and a Content-Security-Policy that blocks every network load
// except images the person chooses to load. This layer removes what has no
// business in an email so that the other two never have to catch it.

/** Elements dropped with everything inside them. */
const DROP = new Set([
  "script", "noscript", "iframe", "frame", "frameset", "object", "embed", "applet",
  "form", "input", "button", "select", "textarea", "option", "datalist", "output",
  "meta", "link", "base", "title", "head", "template", "slot", "portal",
  "audio", "video", "source", "track", "canvas", "math", "dialog", "param",
]);

/** Attributes that carry a URL. */
const URL_ATTRS = new Set(["href", "src", "xlink:href", "action", "formaction", "background", "poster", "cite", "longdesc", "lowsrc", "dynsrc", "usemap", "srcset", "ping"]);

/** Never kept, whatever their value. */
const DROP_ATTRS = new Set(["srcdoc", "formaction", "action", "ping", "http-equiv", "autofocus", "contenteditable", "draggable", "is", "nonce", "integrity", "srcset", "usemap"]);

const SAFE_LINK = /^(https?:|mailto:|#)/i;
const SAFE_IMG = /^(https?:|data:image\/(png|gif|jpe?g|webp|bmp);)/i;

/** `expression()`, `javascript:` and `@import` in inline CSS. */
function cleanStyle(css: string): string | null {
  const flat = css.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\\/g, "");
  if (/expression\s*\(|javascript:|vbscript:|@import|behavior\s*:|-moz-binding/i.test(flat)) return null;
  return css;
}

/** Strips scripts, handlers, forms, embeds and unsafe URLs. Returns body HTML. */
export function sanitizeEmail(html: string): string {
  // DOMParser documents are inert: nothing in them loads or runs.
  const doc = new DOMParser().parseFromString(html, "text/html");
  const walk = (el: Element) => {
    for (const child of Array.from(el.children)) {
      const tag = child.tagName.toLowerCase();
      if (DROP.has(tag) || (child.namespaceURI !== "http://www.w3.org/1999/xhtml" && tag !== "svg" && !(child.closest("svg")))) {
        child.remove();
        continue;
      }
      // SVG can carry scripts and animations that rewrite links.
      if (tag === "svg") {
        child.querySelectorAll("script, foreignObject, animate, set, animateTransform, animateMotion, use, a").forEach((n) => n.remove());
      }
      for (const attr of Array.from(child.attributes)) {
        const name = attr.name.toLowerCase();
        const value = attr.value.trim().replace(/[\u0000- ]/g, "");
        if (name.startsWith("on") || DROP_ATTRS.has(name)) {
          child.removeAttribute(attr.name);
        } else if (URL_ATTRS.has(name)) {
          const ok = name === "href" ? SAFE_LINK.test(value) : name === "src" && tag === "img" ? SAFE_IMG.test(value) : name === "background" ? SAFE_IMG.test(value) : false;
          if (!ok) child.removeAttribute(attr.name);
        } else if (name === "style") {
          const css = cleanStyle(attr.value);
          if (css === null) child.removeAttribute(attr.name);
        }
      }
      if (tag === "a") {
        child.setAttribute("rel", "noopener noreferrer");
        const href = child.getAttribute("href");
        if (href && !href.startsWith("#")) child.setAttribute("title", href);
      }
      if (tag === "style" && cleanStyle(child.textContent ?? "") === null) {
        child.remove();
        continue;
      }
      walk(child);
    }
  };
  // <style> in <head> is common in email; keep it, but cleaned.
  const styles = Array.from(doc.head.querySelectorAll("style"))
    .map((s) => s.textContent ?? "")
    .filter((css) => cleanStyle(css) !== null);
  walk(doc.body);
  return styles.map((css) => `<style>${css.replace(/<\/style/gi, "")}</style>`).join("") + doc.body.innerHTML;
}
