"use client";

import { createContext, useContext } from "react";

import type { RepoInfo } from "@/lib/repo-info";

const RepoInfoContext = createContext<RepoInfo | null>(null);

export function RepoInfoProvider({
  value,
  children,
}: {
  value: RepoInfo;
  children: React.ReactNode;
}) {
  return (
    <RepoInfoContext.Provider value={value}>
      {children}
    </RepoInfoContext.Provider>
  );
}

export function useRepoInfo(): RepoInfo {
  const ctx = useContext(RepoInfoContext);
  if (!ctx) {
    throw new Error("useRepoInfo must be used inside <RepoInfoProvider>");
  }
  return ctx;
}
