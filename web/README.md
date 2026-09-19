This is a [Next.js](https://nextjs.org) project bootstrapped with [`create-next-app`](https://nextjs.org/docs/app/api-reference/cli/create-next-app).

## Getting Started

First, run the development server:

```bash
npm run dev
# or
yarn dev
# or
pnpm dev
# or
bun dev
```

Open [http://localhost:3000](http://localhost:3000) with your browser to see the result.

You can start editing the page by modifying `app/page.tsx`. The page auto-updates as you edit the file.

This project uses [`next/font`](https://nextjs.org/docs/app/building-your-application/optimizing/fonts) to automatically optimize and load [Geist](https://vercel.com/font), a new font family for Vercel.

## Keyword links in docs

Curated keywords render as links in docs prose automatically. Nothing in the
markdown changes — the pass runs at render time, so the source stays plain
markdown for the readers that consume it raw (agents, the skill corpus) and for
GitHub.

Add a term to `src/lib/keyword-links.ts`:

```ts
{ term: "RedEX", slug: "guides/durable-logs" },
```

- Only the **first occurrence per page** is linked; later mentions stay plain.
- Matching is **case-insensitive** and **exact otherwise** — `RedEX`, `Redex`,
  and `redex` all link, but one map entry per term (a second casing is a build
  error). The link text keeps the spelling as written in the prose.
- Word boundaries are enforced, so `RedEX` does not match inside `RedEXFile` and
  `MCP` does not match inside `MCPs`. When one term contains another
  (`MCP bridge` / `MCP`) the longer one wins.
- Links inside code, inline code, headings, and existing links are never
  rewritten; a page never links to itself.
- `slug` must resolve. `assertKeywordLinksResolve` runs from
  `generateStaticParams`, so a renamed page fails the build instead of leaving a
  dead keyword — the map is data, not markdown, so `npm run check:docs` cannot
  see it.

The index that resolves a slug to its URL and title is `src/lib/doc-index.ts`;
the renderer is `src/lib/remark-keyword-links.ts`.

## Learn More

To learn more about Next.js, take a look at the following resources:

- [Next.js Documentation](https://nextjs.org/docs) - learn about Next.js features and API.
- [Learn Next.js](https://nextjs.org/learn) - an interactive Next.js tutorial.

You can check out [the Next.js GitHub repository](https://github.com/vercel/next.js) - your feedback and contributions are welcome!

## Deploy on Vercel

The easiest way to deploy your Next.js app is to use the [Vercel Platform](https://vercel.com/new?utm_medium=default-template&filter=next.js&utm_source=create-next-app&utm_campaign=create-next-app-readme) from the creators of Next.js.

Check out our [Next.js deployment documentation](https://nextjs.org/docs/app/building-your-application/deploying) for more details.
