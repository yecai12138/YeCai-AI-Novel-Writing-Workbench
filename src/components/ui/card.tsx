import type { ComponentPropsWithoutRef } from "react";
import { forwardRef } from "react";

import { cn } from "@/lib/cn";

/** shadcn/ui 的 Card，配色映射到本项目的 ink / accent 语义色。 */
export const Card = forwardRef<HTMLDivElement, ComponentPropsWithoutRef<"div">>(
  function Card({ className, ...props }, ref) {
    return (
      <div
        ref={ref}
        className={cn("rounded-lg border border-ink-200 bg-white text-ink-900 shadow-sm", className)}
        {...props}
      />
    );
  },
);

export const CardHeader = forwardRef<HTMLDivElement, ComponentPropsWithoutRef<"div">>(
  function CardHeader({ className, ...props }, ref) {
    return <div ref={ref} className={cn("flex flex-col gap-1 p-3", className)} {...props} />;
  },
);

export const CardTitle = forwardRef<HTMLDivElement, ComponentPropsWithoutRef<"div">>(
  function CardTitle({ className, ...props }, ref) {
    return (
      <div
        ref={ref}
        className={cn("text-sm font-semibold leading-none text-ink-900", className)}
        {...props}
      />
    );
  },
);

export const CardDescription = forwardRef<HTMLParagraphElement, ComponentPropsWithoutRef<"p">>(
  function CardDescription({ className, ...props }, ref) {
    return <p ref={ref} className={cn("text-xs text-ink-500", className)} {...props} />;
  },
);

export const CardContent = forwardRef<HTMLDivElement, ComponentPropsWithoutRef<"div">>(
  function CardContent({ className, ...props }, ref) {
    return <div ref={ref} className={cn("p-3 pt-0", className)} {...props} />;
  },
);

export const CardFooter = forwardRef<HTMLDivElement, ComponentPropsWithoutRef<"div">>(
  function CardFooter({ className, ...props }, ref) {
    return <div ref={ref} className={cn("flex items-center gap-2 p-3 pt-0", className)} {...props} />;
  },
);
