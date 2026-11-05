// The component library. Every screen and every plugin surface builds from
// these, so new features look like the rest of the app by default.

import type { ButtonHTMLAttributes, InputHTMLAttributes, ReactNode, Ref, TextareaHTMLAttributes } from "react";
import type { Tone, Urgency } from "../lib/types";
import { Icon } from "./icons";

const cx = (...c: (string | false | null | undefined)[]) => c.filter(Boolean).join(" ");

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "secondary" | "ghost";
  size?: "sm" | "md";
  icon?: string;
  shortcut?: string;
};

export function Button({ variant = "secondary", size = "md", icon, shortcut, children, className, ...rest }: ButtonProps) {
  return (
    <button className={cx("btn", `btn-${variant}`, `btn-${size}`, className)} {...rest}>
      {icon && <Icon name={icon} size={size === "sm" ? 14 : 15} />}
      {children && <span>{children}</span>}
      {shortcut && <Kbd>{shortcut}</Kbd>}
    </button>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="kbd">{children}</kbd>;
}

export function Badge({ tone = "neutral", icon, children, title }: { tone?: Tone; icon?: string; children: ReactNode; title?: string }) {
  return (
    <span className={cx("badge", `tone-${tone}`)} title={title}>
      {icon && <Icon name={icon} size={12} />}
      {children}
    </span>
  );
}

/** A colored glyph for a source/kind. */
export function SourceIcon({ icon, tone = "neutral", size = 28 }: { icon: string; tone?: Tone; size?: number }) {
  return (
    <span className={cx("source-icon", `tone-${tone}`)} style={{ width: size, height: size }}>
      <Icon name={icon} size={Math.round(size * 0.55)} />
    </span>
  );
}

const urgencyLabel: Record<Urgency, string> = { now: "Now", today: "Today", this_week: "This week", whenever: "Whenever" };

/** The score and urgency of a ranked item. */
export function ScorePill({ score, urgency, pending }: { score?: number; urgency?: Urgency; pending?: boolean }) {
  if (pending || score === undefined || !urgency) return <span className="score-pill pending" title="Ranking…">··</span>;
  return (
    <span className={cx("score-pill", `urgency-${urgency}`)} title={`${urgencyLabel[urgency]} · score ${score}`}>
      {score}
    </span>
  );
}

export function UrgencyLabel({ urgency }: { urgency: Urgency }) {
  return <span className={cx("urgency-label", `urgency-${urgency}`)}>{urgencyLabel[urgency]}</span>;
}

export function Card({ children, className, padded = true }: { children: ReactNode; className?: string; padded?: boolean }) {
  return <div className={cx("card", padded && "card-padded", className)}>{children}</div>;
}

export function Section({ title, description, actions, children }: { title: string; description?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return (
    <section className="section">
      <header className="section-header">
        <div>
          <h2 className="section-title">{title}</h2>
          {description && <p className="section-description">{description}</p>}
        </div>
        {actions && <div className="section-actions">{actions}</div>}
      </header>
      {children}
    </section>
  );
}

export function PageHeader({ title, subtitle, actions }: { title: string; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <header className="page-header">
      <div>
        <h1 className="page-title">{title}</h1>
        {subtitle && <p className="page-subtitle">{subtitle}</p>}
      </div>
      {actions && <div className="page-actions">{actions}</div>}
    </header>
  );
}

export function EmptyState({ icon, title, children }: { icon: string; title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <Icon name={icon} size={28} />
      <h3>{title}</h3>
      {children && <p>{children}</p>}
    </div>
  );
}

export function TextInput({ icon, ref, ...rest }: InputHTMLAttributes<HTMLInputElement> & { icon?: string; ref?: Ref<HTMLInputElement> }) {
  return (
    <label className={cx("input", icon && "input-with-icon")}>
      {icon && <Icon name={icon} size={14} />}
      <input ref={ref} {...rest} />
    </label>
  );
}

export function TextArea(props: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea className="textarea" {...props} />;
}

export function Toggle({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <button type="button" role="switch" aria-checked={checked} aria-label={label} className={cx("toggle", checked && "on")} onClick={() => onChange(!checked)}>
      <span />
    </button>
  );
}

export function FormRow({ label, help, children }: { label: string; help?: ReactNode; children: ReactNode }) {
  return (
    <div className="form-row">
      <div className="form-label">
        <span>{label}</span>
        {help && <small>{help}</small>}
      </div>
      <div className="form-control">{children}</div>
    </div>
  );
}

export function StatusDot({ tone }: { tone: Tone }) {
  return <span className={cx("status-dot", `tone-${tone}`)} />;
}

/** A thin bar for "remaining of limit" style values. */
export function Meter({ value, max, tone = "accent" }: { value: number; max: number; tone?: Tone }) {
  const pct = max > 0 ? Math.max(0, Math.min(100, (value / max) * 100)) : 0;
  return (
    <div className="meter">
      <div className={cx("meter-fill", `tone-${tone}`)} style={{ width: `${pct}%` }} />
    </div>
  );
}

export function Tabs<T extends string>({ value, onChange, items }: { value: T; onChange: (v: T) => void; items: { value: T; label: string; count?: number }[] }) {
  return (
    <div className="tabs" role="tablist">
      {items.map((i) => (
        <button key={i.value} role="tab" aria-selected={value === i.value} className={cx("tab", value === i.value && "active")} onClick={() => onChange(i.value)}>
          {i.label}
          {i.count !== undefined && <span className="tab-count">{i.count}</span>}
        </button>
      ))}
    </div>
  );
}

export { cx };
