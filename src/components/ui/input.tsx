import { forwardRef, type InputHTMLAttributes } from "react";

import { cn } from "@/lib/cn";

/**
 * shadcn/ui `Input` 结构，配色映射到本项目的 ink/accent 语义色。
 */
export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(
  function Input({ className, type = "text", ...props }, ref) {
    return (
      <input
        ref={ref}
        type={type}
        className={cn(
          "flex h-9 w-full rounded-md border border-ink-200 bg-white px-3 py-1 text-sm text-ink-900 shadow-sm",
          "transition-colors placeholder:text-ink-400",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40 focus-visible:border-accent/60",
          "disabled:cursor-not-allowed disabled:bg-ink-50 disabled:text-ink-400",
          className,
        )}
        {...props}
      />
    );
  },
);
