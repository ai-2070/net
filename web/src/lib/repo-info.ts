import "server-only";

import { marked } from "marked";

marked.setOptions({ async: false, gfm: true, breaks: false });

export function renderMarkdown(text: string): string {
  if (!text) return "";
  try {
    const html = marked.parse(text);
    return typeof html === "string" ? html : "";
  } catch {
    return "";
  }
}

export interface Release {
  tag: string;
  title: string;
  codename: string | null;
  bodyHtml: string;
  publishedAt: string;
  htmlUrl: string;
  prerelease: boolean;
}

export interface RepoInfo {
  version: string;
  codename: string;
  buildDate: string;
  sha: string;
  releases: ReadonlyArray<Release>;
}

export const REPO = "ai-2070/net";

export const FALLBACK: RepoInfo = {
  version: "v0.0.0",
  codename: "—",
  buildDate: "—",
  sha: "0000000",
  releases: [],
};

export interface GhCommit {
  sha?: string;
  commit?: { committer?: { date?: string } };
}

export interface GhRelease {
  tag_name?: string;
  name?: string | null;
  body?: string | null;
  published_at?: string | null;
  html_url?: string;
  prerelease?: boolean;
}

export function extractCodename(
  title: string | undefined | null,
): string | null {
  if (!title) return null;
  const m = title.match(/["“”]([^"“”]+)["“”]/);
  return m && m[1] ? m[1] : null;
}

export function ghHeaders(): HeadersInit {
  const headers: Record<string, string> = {
    Accept: "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28",
  };
  if (process.env.GITHUB_TOKEN) {
    headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  }
  return headers;
}

export function formatDate(iso: string): string {
  return iso.slice(0, 10).replace(/-/g, ".");
}

export async function fetchLatestRelease(): Promise<{
  tag: string;
  codename: string | null;
} | null> {
  try {
    const res = await fetch(
      `https://api.github.com/repos/${REPO}/releases/latest`,
      { headers: ghHeaders(), next: { revalidate: 3600 } },
    );
    if (!res.ok) return null;
    const data = (await res.json()) as GhRelease;
    if (!data.tag_name) return null;
    return {
      tag: data.tag_name,
      codename: extractCodename(data.name),
    };
  } catch {
    return null;
  }
}

export async function fetchHeadCommit(): Promise<{
  sha: string;
  date: string;
} | null> {
  try {
    const res = await fetch(
      `https://api.github.com/repos/${REPO}/commits/HEAD`,
      { headers: ghHeaders(), next: { revalidate: 3600 } },
    );
    if (!res.ok) return null;
    const data = (await res.json()) as GhCommit;
    const sha = (data.sha ?? "").slice(0, 7);
    const date = data.commit?.committer?.date ?? "";
    if (!sha || !date) return null;
    return { sha, date };
  } catch {
    return null;
  }
}

export async function fetchAllReleases(): Promise<ReadonlyArray<Release>> {
  try {
    const res = await fetch(
      `https://api.github.com/repos/${REPO}/releases?per_page=100`,
      { headers: ghHeaders(), next: { revalidate: 3600 } },
    );
    if (!res.ok) return [];
    const data = (await res.json()) as ReadonlyArray<GhRelease>;
    return data.flatMap<Release>((r) => {
      if (!r.tag_name) return [];
      const md = (r.body ?? "").replace(/\r\n/g, "\n").trim();
      return [
        {
          tag: r.tag_name,
          title: r.name ?? r.tag_name,
          codename: extractCodename(r.name),
          bodyHtml: renderMarkdown(md),
          publishedAt: r.published_at ?? "",
          htmlUrl:
            r.html_url ??
            `https://github.com/${REPO}/releases/tag/${r.tag_name}`,
          prerelease: r.prerelease ?? false,
        },
      ];
    });
  } catch {
    return [];
  }
}

export async function getRepoInfo(): Promise<RepoInfo> {
  const [release, head, releases] = await Promise.all([
    fetchLatestRelease(),
    fetchHeadCommit(),
    fetchAllReleases(),
  ]);
  return {
    version: release?.tag ?? FALLBACK.version,
    codename: release?.codename ?? FALLBACK.codename,
    buildDate: head ? formatDate(head.date) : FALLBACK.buildDate,
    sha: head ? head.sha : FALLBACK.sha,
    releases,
  };
}
