import type { InputHTMLAttributes, ReactNode } from "react";

import { cn } from "@/lib/cn";

interface FieldProps {
  label: string;
  hint?: ReactNode;
  error?: string | null;
  children: ReactNode;
}

export function Field({ label, hint, error, children }: FieldProps) {
  return (
    <label className="block space-y-1.5">
      <span className="flex items-baseline justify-between gap-2">
        <span className="text-xs font-medium text-ink-700">{label}</span>
        {hint && <span className="text-[11px] text-ink-400">{hint}</span>}
      </span>
      {children}
      {error && <span className="block text-[11px] text-rose-600">{error}</span>}
    </label>
  );
}

export function TextInput({
  className,
  ...rest
}: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      className={cn(
        "h-9 w-full rounded-md border border-ink-200 bg-white px-2.5 text-sm text-ink-900",
        "placeholder:text-ink-300 focus:border-accent focus:outline-none focus:ring-2 focus:ring-accent/25",
        "disabled:bg-ink-50 disabled:text-ink-400",
        className,
      )}
      {...rest}
    />
  );
}

export function Badge({
  tone = "neutral",
  children,
}: {
  tone?: "neutral" | "ok" | "warn" | "error";
  children: ReactNode;
}) {
  const tones = {
    neutral: "border-ink-200 bg-ink-50 text-ink-600",
    ok: "border-emerald-200 bg-emerald-50 text-emerald-700",
    warn: "border-amber-200 bg-amber-50 text-amber-700",
    error: "border-rose-200 bg-rose-50 text-rose-700",
  } as const;
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-full border px-2 py-0.5 text-[11px] font-medium",
        tones[tone],
      )}
    >
      {children}
    </span>
  );
}
