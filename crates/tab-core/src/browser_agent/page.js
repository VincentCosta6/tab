// tab's isolated-world script for live pages: see page.rs and
// docs/agent-browser.md. page.rs puts the op in, as JSON, below.
(() => {
try {
const OP = __OP__;
const g = globalThis;
if (!g.__tab || g.__tab.doc !== document) {
  g.__tab = { doc: document, refs: new Map(), els: new WeakMap(), next: 1, touched: 0 };
  const mark = (e) => { if (e.isTrusted) g.__tab.touched = Date.now(); };
  addEventListener("pointerdown", mark, true);
  addEventListener("keydown", mark, true);
}
const T = g.__tab;
const visible = (el) => {
  const r = el.getBoundingClientRect();
  if (r.width === 0 && r.height === 0) return false;
  const s = getComputedStyle(el);
  return s.visibility !== "hidden" && s.display !== "none" && s.opacity !== "0";
};
const SECRET_AC = ["current-password", "new-password", "one-time-code", "cc-number", "cc-csc", "cc-exp"];
const isSecret = (el) => {
  if (el.tagName === "INPUT" && el.type === "password") return true;
  const ac = (el.getAttribute("autocomplete") || "").toLowerCase();
  if (SECRET_AC.some((a) => ac.includes(a))) return true;
  const n = ((el.name || "") + " " + (el.id || "")).toLowerCase();
  return /(passw|passcode|otp|totp|2fa|mfa|cvc|cvv|card.?number|secret|api.?key|token)/.test(n);
};
const text = (s) => (s || "").replace(/\s+/g, " ").trim().slice(0, 200);
const labelsOf = (el) => {
  const out = [];
  const add = (s) => { s = text(s); if (s && !out.includes(s)) out.push(s); };
  add(el.getAttribute("aria-label"));
  const lb = el.getAttribute("aria-labelledby");
  if (lb) add(lb.split(/\s+/).map((id) => document.getElementById(id)?.innerText || "").join(" "));
  if (el.labels) for (const l of el.labels) add(l.innerText);
  if (el.tagName === "INPUT" && ["submit", "button", "reset"].includes(el.type)) add(el.value);
  if (!["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName)) add(el.innerText);
  add(el.getAttribute("placeholder"));
  add(el.getAttribute("title"));
  add(el.getAttribute("name"));
  return out;
};
const INTERACTIVE = "a[href],button,input:not([type=hidden]),textarea,select,summary,[role=button],[role=link],[role=tab],[role=checkbox],[role=radio],[role=switch],[role=menuitem],[role=option],[role=combobox],[role=searchbox],[role=textbox],[contenteditable=''],[contenteditable=true]";
const all = (root, out) => {
  for (const el of root.querySelectorAll(INTERACTIVE)) out.push(el);
  for (const host of root.querySelectorAll("*")) if (host.shadowRoot) all(host.shadowRoot, out);
  return out;
};
const describe = (el, id) => {
  const tag = el.tagName.toLowerCase();
  const role = el.getAttribute("role") || "";
  const kind = (el.getAttribute("type") || (tag === "button" ? "submit" : "")).toLowerCase();
  const form = el.form || el.closest("form");
  const submits = (tag === "button" && kind === "submit" && !!form) || (tag === "input" && ["submit", "image"].includes(kind));
  const secret = isSecret(el);
  const search = kind === "search" || role === "searchbox" || /search|query|^q$/i.test((el.name || "") + " " + (el.id || "")) || !!el.closest("[role=search]");
  let href = null;
  if (tag === "a" && el.href && /^https?:/i.test(el.href)) href = el.href.split("#")[0] || el.href;
  const toggles = role === "tab" || tag === "summary" || el.hasAttribute("aria-expanded");
  let value = null;
  if (["input", "textarea", "select"].includes(tag) && !secret && kind !== "hidden") {
    value = tag === "select" ? (el.selectedOptions[0]?.innerText || "") : text(el.value);
  }
  return { ref: id, tag, role, kind, labels: labelsOf(el), secret, search, href, submits, toggles, value, disabled: !!el.disabled };
};
const refFor = (el) => {
  let id = T.els.get(el);
  if (!id) { id = "e" + T.next++; T.els.set(el, id); T.refs.set(id, new WeakRef(el)); }
  return id;
};
const byRef = (id) => {
  const el = T.refs.get(id)?.deref();
  if (!el || !el.isConnected) throw new Error("element " + id + " is gone; read the page again");
  return el;
};
const out = (v) => JSON.stringify(v);
switch (OP.op) {
  case "snapshot": {
    const els = all(document, []).filter(visible).slice(0, 400).map((el) => describe(el, refFor(el)));
    let body = document.body ? document.body.innerText : "";
    for (const el of all(document, [])) if (isSecret(el) && el.value) body = body.split(el.value).join("[secret hidden]");
    const max = OP.max_chars || 12000;
    return out({ url: location.href, title: document.title, text: body.slice(0, max), truncated: body.length > max, elements: els });
  }
  case "describe": return out(describe(byRef(OP.ref), OP.ref));
  case "click": {
    const el = byRef(OP.ref);
    el.scrollIntoView({ block: "center" });
    el.focus?.();
    el.click();
    return out({ ok: true });
  }
  case "type": {
    const el = byRef(OP.ref);
    if (isSecret(el)) throw new Error("secret field");
    el.scrollIntoView({ block: "center" });
    el.focus();
    if (el.isContentEditable) {
      el.textContent = OP.text;
    } else {
      const proto = el.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(proto, "value").set.call(el, OP.text);
    }
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return out({ ok: true });
  }
  case "select": {
    const el = byRef(OP.ref);
    const opt = [...(el.options || [])].find((o) => text(o.innerText) === OP.label || o.value === OP.label);
    if (!opt) throw new Error("no option " + OP.label);
    el.value = opt.value;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
    return out({ ok: true });
  }
  case "highlight": {
    for (const old of document.querySelectorAll("[data-tab-highlight]")) old.remove();
    if (!OP.ref) return out({ ok: true });
    const el = byRef(OP.ref);
    el.scrollIntoView({ block: "center" });
    const r = el.getBoundingClientRect();
    const box = document.createElement("div");
    box.setAttribute("data-tab-highlight", "");
    box.style.cssText = `position:fixed;left:${r.left - 4}px;top:${r.top - 4}px;width:${r.width + 8}px;height:${r.height + 8}px;border:3px solid #7c5cff;border-radius:6px;pointer-events:none;z-index:2147483647`;
    document.documentElement.appendChild(box);
    setTimeout(() => box.remove(), 4000);
    return out({ ok: true });
  }
  case "mask": {
    for (const old of document.querySelectorAll("[data-tab-mask]")) old.remove();
    if (OP.on) for (const el of all(document, [])) {
      if (!isSecret(el) || !visible(el)) continue;
      const r = el.getBoundingClientRect();
      const m = document.createElement("div");
      m.setAttribute("data-tab-mask", "");
      m.style.cssText = `position:fixed;left:${r.left}px;top:${r.top}px;width:${r.width}px;height:${r.height}px;background:#222;pointer-events:none;z-index:2147483647`;
      document.documentElement.appendChild(m);
    }
    return out({ ok: true });
  }
  case "touched": return out({ at: T.touched });
  case "ready": return out({ ready: document.readyState === "complete", url: location.href });
  case "go": { location.assign(OP.url); return out({ ok: true }); }
}
throw new Error("unknown op");
} catch (e) {
  return JSON.stringify({ error: String((e && e.message) || e) });
}
})()
