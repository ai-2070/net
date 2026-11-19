import type { Metadata } from "next";
import {
  Archivo,
  JetBrains_Mono,
  Major_Mono_Display,
  Space_Grotesk,
} from "next/font/google";
import localFont from "next/font/local";
import "./globals.css";
import { PostHogProvider } from "@/components/PostHogProvider";
import { RepoInfoProvider } from "@/components/RepoInfoProvider";
import { TopStatusBar } from "@/components/TopStatusBar";
import { getRepoInfo } from "@/lib/repo-info";
import { siteUrl } from "@/lib/site-url";

const jetbrainsMono = JetBrains_Mono({
  variable: "--font-jetbrains-mono",
  subsets: ["latin"],
  weight: ["300", "400", "500", "600", "700", "800"],
});

const majorMonoDisplay = Major_Mono_Display({
  variable: "--font-major-mono-display",
  subsets: ["latin"],
  weight: ["400"],
});

const spaceGrotesk = Space_Grotesk({
  variable: "--font-space-grotesk",
  subsets: ["latin"],
  weight: ["300", "400", "500", "600", "700"],
});

// Landing-page world ("suspended field"): monumental geometric caps for
// titles, a Swiss-lineage grotesque for prose. Scoped to `.world-field`.
const adventor = localFont({
  variable: "--font-adventor",
  src: [
    { path: "../fonts/adventor/texgyreadventor-regular.woff2", weight: "400" },
    { path: "../fonts/adventor/texgyreadventor-bold.woff2", weight: "700" },
  ],
  display: "swap",
});

const archivo = Archivo({
  variable: "--font-archivo",
  subsets: ["latin"],
  weight: ["300", "400", "500", "600"],
});

export const metadata: Metadata = {
  metadataBase: new URL(siteUrl()),
  title: "NET — Network Event Transport. A latency-first encrypted mesh.",
  description:
    "NET is a latency-first encrypted mesh where every device is an equal node. Nanosecond scheduling, zero-copy forwarding, capability-based routing. Open source.",
  // Defaults for pages without their own; no url here, so a page that
  // forgets its own never claims to be the homepage.
  openGraph: { type: "website", siteName: "NET", locale: "en_US" },
  twitter: { card: "summary_large_image" },
};

export default async function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  const repoInfo = await getRepoInfo();

  return (
    <html
      lang="en"
      className={`${jetbrainsMono.variable} ${majorMonoDisplay.variable} ${spaceGrotesk.variable} ${adventor.variable} ${archivo.variable} h-full antialiased`}
    >
      <body className="bg-black">
        <PostHogProvider>
          <RepoInfoProvider value={repoInfo}>
            <TopStatusBar {...repoInfo} />
            {children}
          </RepoInfoProvider>
        </PostHogProvider>
      </body>
    </html>
  );
}
