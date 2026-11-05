import { useEffect, useRef } from "react";
import type { Entity, GraphView } from "../../lib/types";

/**
 * The knowledge graph on a canvas, laid out with a small force simulation:
 * nodes push apart, shared items pull them together, and everything drifts
 * to the middle. A few hundred nodes settle in about a second.
 *
 * Drag the background to pan, scroll to zoom, click a node to select it.
 * The selected node and its neighbours stay bright; the rest dims.
 */

interface Sim {
  e: Entity;
  x: number;
  y: number;
  vx: number;
  vy: number;
  r: number;
}

export function GraphCanvas({ graph, selected, highlight, colorOf, onSelect }: {
  graph: GraphView;
  selected: number | null;
  /** Nodes to emphasize (e.g. search matches), besides the selection. */
  highlight: Set<number>;
  colorOf: (e: Entity) => string;
  onSelect: (id: number | null) => void;
}) {
  const ref = useRef<HTMLCanvasElement>(null);
  // Everything the draw loop reads lives in refs, so it never restarts.
  const nodes = useRef<Sim[]>([]);
  const view = useRef({ x: 0, y: 0, k: 1 });
  const heat = useRef(1);
  const hover = useRef<number | null>(null);
  const props = useRef({ selected, highlight, colorOf, onSelect, graph });
  props.current = { selected, highlight, colorOf, onSelect, graph };

  // New data: keep positions of nodes we already had, place new ones near
  // a neighbour, and reheat.
  useEffect(() => {
    const old = new Map(nodes.current.map((n) => [n.e.id, n]));
    const max = Math.max(1, ...graph.nodes.map((n) => n.item_count));
    const next: Sim[] = graph.nodes.map((e, i) => {
      const prev = old.get(e.id);
      const r = 4 + 14 * Math.sqrt(e.item_count / max);
      if (prev) return { ...prev, e, r };
      const a = i * 2.4;
      const d = 40 + 8 * Math.sqrt(i) * 6;
      return { e, x: Math.cos(a) * d, y: Math.sin(a) * d, vx: 0, vy: 0, r };
    });
    nodes.current = next;
    heat.current = 1;
  }, [graph]);

  useEffect(() => {
    heat.current = Math.max(heat.current, 0.05);
  }, [selected, highlight]);

  useEffect(() => {
    const canvas = ref.current!;
    const ctx = canvas.getContext("2d")!;
    let raf = 0;
    let size = { w: 0, h: 0 };

    const resize = () => {
      const r = canvas.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      size = { w: r.width, h: r.height };
      canvas.width = Math.round(r.width * dpr);
      canvas.height = Math.round(r.height * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      heat.current = Math.max(heat.current, 0.05);
    };
    const ro = new ResizeObserver(resize);
    ro.observe(canvas);
    resize();

    const css = (name: string) => getComputedStyle(canvas).getPropertyValue(name).trim();

    const step = () => {
      const ns = nodes.current;
      const t = heat.current;
      if (t < 0.002) return;
      const byId = new Map(ns.map((n) => [n.e.id, n]));
      // Repulsion (all pairs; fine at this size).
      for (let i = 0; i < ns.length; i++) {
        const a = ns[i];
        for (let j = i + 1; j < ns.length; j++) {
          const b = ns[j];
          let dx = a.x - b.x;
          let dy = a.y - b.y;
          let d2 = dx * dx + dy * dy;
          if (d2 < 1) (dx = Math.random() - 0.5), (dy = Math.random() - 0.5), (d2 = 1);
          const f = (1800 * t) / d2;
          const d = Math.sqrt(d2);
          a.vx += (dx / d) * f;
          a.vy += (dy / d) * f;
          b.vx -= (dx / d) * f;
          b.vy -= (dy / d) * f;
        }
      }
      // Springs along shared items.
      for (const ed of props.current.graph.edges) {
        const a = byId.get(ed.src);
        const b = byId.get(ed.dst);
        if (!a || !b) continue;
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const d = Math.sqrt(dx * dx + dy * dy) || 1;
        const want = 50 + a.r + b.r;
        const f = ((d - want) / d) * 0.04 * Math.min(3, 1 + Math.log(ed.weight)) * t;
        a.vx += dx * f;
        a.vy += dy * f;
        b.vx -= dx * f;
        b.vy -= dy * f;
      }
      for (const n of ns) {
        n.vx -= n.x * 0.004 * t;
        n.vy -= n.y * 0.004 * t;
        n.x += n.vx;
        n.y += n.vy;
        n.vx *= 0.6;
        n.vy *= 0.6;
      }
      heat.current = t * 0.985;
    };

    const draw = () => {
      for (let i = 0; i < 2; i++) step();
      const { w, h } = size;
      const v = view.current;
      const { selected, highlight, colorOf } = props.current;
      ctx.clearRect(0, 0, w, h);
      ctx.save();
      ctx.translate(w / 2 + v.x, h / 2 + v.y);
      ctx.scale(v.k, v.k);

      const ns = nodes.current;
      const byId = new Map(ns.map((n) => [n.e.id, n]));
      const focus = selected ?? hover.current;
      const near = new Set<number>();
      if (focus != null) {
        near.add(focus);
        for (const ed of props.current.graph.edges) {
          if (ed.src === focus) near.add(ed.dst);
          if (ed.dst === focus) near.add(ed.src);
        }
      }
      const lit = (id: number) => (focus == null && !highlight.size) || near.has(id) || highlight.has(id);

      const line = css("--border-strong") || "#888";
      const fg = css("--fg") || "#111";
      const subtle = css("--fg-subtle") || "#888";
      const bg = css("--bg") || "#fff";

      for (const ed of props.current.graph.edges) {
        const a = byId.get(ed.src);
        const b = byId.get(ed.dst);
        if (!a || !b) continue;
        const on = focus != null && (ed.src === focus || ed.dst === focus);
        ctx.globalAlpha = on ? 0.9 : lit(a.e.id) && lit(b.e.id) ? 0.35 : 0.08;
        ctx.strokeStyle = on ? colorOf(byId.get(focus!)!.e) : line;
        ctx.lineWidth = (on ? 1.5 : 1) * Math.min(4, 0.6 + Math.log2(1 + ed.weight)) / v.k;
        ctx.beginPath();
        ctx.moveTo(a.x, a.y);
        ctx.lineTo(b.x, b.y);
        ctx.stroke();
      }

      for (const n of ns) {
        const on = lit(n.e.id);
        ctx.globalAlpha = on ? 1 : 0.2;
        ctx.fillStyle = colorOf(n.e);
        ctx.beginPath();
        if (n.e.kind === "project") {
          const s = n.r * 1.15;
          ctx.roundRect(n.x - s, n.y - s, s * 2, s * 2, s * 0.45);
        } else ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2);
        ctx.fill();
        if (n.e.id === selected || highlight.has(n.e.id)) {
          ctx.lineWidth = 2.5 / v.k;
          ctx.strokeStyle = fg;
          ctx.stroke();
        }
      }

      // Labels: big nodes, and everything near the focus.
      ctx.textAlign = "center";
      ctx.textBaseline = "top";
      for (const n of ns) {
        const show = near.has(n.e.id) || highlight.has(n.e.id) || n.e.kind === "project" || (n.r > 11 && focus == null) || v.k > 1.6;
        if (!show) continue;
        const strong = n.e.id === focus || n.e.kind === "project";
        ctx.globalAlpha = lit(n.e.id) ? 1 : 0.25;
        ctx.font = `${strong ? 600 : 500} ${12 / v.k}px Inter, system-ui, sans-serif`;
        const label = n.e.label.length > 28 ? `${n.e.label.slice(0, 27)}…` : n.e.label;
        ctx.lineWidth = 3 / v.k;
        ctx.strokeStyle = bg;
        ctx.strokeText(label, n.x, n.y + n.r + 4 / v.k);
        ctx.fillStyle = strong ? fg : subtle;
        ctx.fillText(label, n.x, n.y + n.r + 4 / v.k);
      }
      ctx.restore();
      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);

    // Screen to graph coordinates.
    const at = (ev: MouseEvent) => {
      const r = canvas.getBoundingClientRect();
      const v = view.current;
      return { x: (ev.clientX - r.left - r.width / 2 - v.x) / v.k, y: (ev.clientY - r.top - r.height / 2 - v.y) / v.k };
    };
    const hit = (ev: MouseEvent) => {
      const p = at(ev);
      let best: Sim | null = null;
      for (const n of nodes.current) {
        const d = Math.hypot(n.x - p.x, n.y - p.y);
        if (d <= n.r + 4 / view.current.k && (!best || n.r > best.r)) best = n;
      }
      return best;
    };

    let drag: { x: number; y: number; vx: number; vy: number; node: Sim | null; moved: boolean } | null = null;
    const down = (ev: MouseEvent) => {
      const node = hit(ev);
      drag = { x: ev.clientX, y: ev.clientY, vx: view.current.x, vy: view.current.y, node, moved: false };
    };
    const move = (ev: MouseEvent) => {
      if (!drag) {
        const n = hit(ev);
        hover.current = n?.e.id ?? null;
        canvas.style.cursor = n ? "pointer" : "grab";
        return;
      }
      const dx = ev.clientX - drag.x;
      const dy = ev.clientY - drag.y;
      if (Math.abs(dx) + Math.abs(dy) > 3) drag.moved = true;
      if (drag.node) {
        const p = at(ev);
        drag.node.x = p.x;
        drag.node.y = p.y;
        heat.current = Math.max(heat.current, 0.08);
      } else {
        view.current.x = drag.vx + dx;
        view.current.y = drag.vy + dy;
        canvas.style.cursor = "grabbing";
      }
    };
    const up = () => {
      if (drag && !drag.moved) props.current.onSelect(drag.node?.e.id ?? null);
      drag = null;
    };
    const leave = () => (hover.current = null);
    const wheel = (ev: WheelEvent) => {
      ev.preventDefault();
      const v = view.current;
      const r = canvas.getBoundingClientRect();
      const mx = ev.clientX - r.left - r.width / 2;
      const my = ev.clientY - r.top - r.height / 2;
      const k = Math.min(4, Math.max(0.2, v.k * Math.exp(-ev.deltaY * 0.0015)));
      v.x = mx - ((mx - v.x) * k) / v.k;
      v.y = my - ((my - v.y) * k) / v.k;
      v.k = k;
    };
    canvas.addEventListener("mousedown", down);
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    canvas.addEventListener("mouseleave", leave);
    canvas.addEventListener("wheel", wheel, { passive: false });
    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      canvas.removeEventListener("mousedown", down);
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      canvas.removeEventListener("mouseleave", leave);
      canvas.removeEventListener("wheel", wheel);
    };
  }, []);

  return <canvas ref={ref} className="graph-canvas" />;
}
