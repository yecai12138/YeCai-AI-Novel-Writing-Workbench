import { cva, type VariantProps } from "class-variance-authority";
import type { ButtonHTMLAttributes } from "react";

import { cn } from "@/lib/cn";

/**
 * shadcn/ui `Button`（cva 变体），配色换成项目语义色。
 * 旧代码里的 `common/Button` 保留，内部转调这里，避免同时存在两套观感。
 */
export const buttonVariants = cva(
  [
    "inline-flex select-none items-center justify-center whitespace-nowrap font-medium transition-colors",
    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40",
    "disabled:cursor-not-allowed disabled:opacity-60",
  ],
  {
    variants: {
      variant: {
        default: "bg-accent text-white shadow-sm hover:bg-accent-strong",
        secondary: "border border-ink-200 bg-white text-ink-800 shadow-sm hover:bg-ink-50",
        ghost: "text-ink-600 hover:bg-ink-100 hover:text-ink-900",
        outline: "border border-ink-300 bg-transparent text-ink-800 hover:bg-ink-50",
        destructive: "bg-rose-600 text-white shadow-sm hover:bg-rose-700",
        link: "text-accent underline-offset-4 hover:underline",
      },
      size: {
        sm: "h-7 gap-1.5 rounded-md px-2.5 text-xs",
        default: "h-9 gap-2 rounded-md px-3.5 text-sm",
        lg: "h-10 gap-2 rounded-md px-5 text-sm",
        icon: "h-8 w-8 rounded-md",
      },
    },
    defaultVariants: { variant: "default", size: "default" },
  },
);

export interface ButtonProps
  extends ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {}

export function Button({ className, variant, size, type = "button", ...props }: ButtonProps) {
  return <button type={type} className={cn(buttonVariants({ variant, size }), className)} {...props} />;
}
