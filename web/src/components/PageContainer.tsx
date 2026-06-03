import React from "react";
import { cn } from "@/lib/cn";

export function PageContainer({
  children,
  className = "",
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        `bg-bg text-ink overflow-x-hidden font-mono min-h-full`,
        className,
      )}
    >
      {children}
    </div>
  );
}
