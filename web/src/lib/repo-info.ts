import "server-only";

export interface RepoInfo {
  version: string;
  buildDate: string;
  sha: string;
}

const REPO = "ai-2070/net";

const FALLBACK: RepoInfo = {
  version: "v0.0.0",
  buildDate: "—",
  sha: "0000000",
};

interface GhCommit {
  sha?: string;
  commit?: { committer?: { date?: string } };
}

interface GhRelease {
  tag_name?: string;
}

function ghHeaders(): HeadersInit {
  const headers: Record<string, string> = {
    Accept: "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28",
  };
  if (process.env.GITHUB_TOKEN) {
    headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  }
  return headers;
}

function formatDate(iso: string): string {
  return iso.slice(0, 10).replace(/-/g, ".");
}

async function fetchLatestRelease(): Promise<string | null> {
  try {
    const res = await fetch(
      `https://api.github.com/repos/${REPO}/releases/latest`,
      { headers: ghHeaders(), next: { revalidate: 3600 } },
    );
    if (!res.ok) return null;
    const data = (await res.json()) as GhRelease;
    return data.tag_name ?? null;
  } catch {
    return null;
  }
}

async function fetchHeadCommit(): Promise<{
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

export async function getRepoInfo(): Promise<RepoInfo> {
  const [tag, head] = await Promise.all([
    fetchLatestRelease(),
    fetchHeadCommit(),
  ]);
  return {
    version: tag ?? FALLBACK.version,
    buildDate: head ? formatDate(head.date) : FALLBACK.buildDate,
    sha: head ? head.sha : FALLBACK.sha,
  };
}
