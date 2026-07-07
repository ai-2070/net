"use client";

import { useEffect } from "react";
import posthog from "posthog-js";
import { PostHogProvider as PHProvider } from "@posthog/react";

export function PostHogProvider({ children }: { children: React.ReactNode }) {
  useEffect(() => {
    const token = process.env.NEXT_PUBLIC_POSTHOG_PROJECT_TOKEN;
    if (!token) {
      if (process.env.NODE_ENV !== "production") {
        console.info(
          "[PostHog] NEXT_PUBLIC_POSTHOG_PROJECT_TOKEN is not set — analytics disabled.",
        );
      }
      return;
    }
    posthog.init(token, {
      api_host: "/ingest",
      defaults: "2026-01-30",
    });
  }, []);

  return <PHProvider client={posthog}>{children}</PHProvider>;
}
