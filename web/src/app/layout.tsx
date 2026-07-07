import type { Metadata } from "next";
import {
  JetBrains_Mono,
  Major_Mono_Display,
  Space_Grotesk,
} from "next/font/google";
import "./globals.css";
import { PostHogProvider } from "@/components/PostHogProvider";
import { RepoInfoProvider } from "@/components/RepoInfoProvider";
import { TopStatusBar } from "@/components/TopStatusBar";
import { getRepoInfo } from "@/lib/repo-info";

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

export const metadata: Metadata = {
  title: "NET — Network Event Transport. A distributed mesh runtime.",
  description:
    "NET is a distributed mesh runtime. Your laptop, phone, sensor, robot, satellite — each advertises what it can do and finds what it needs. Nanosecond scheduling, zero-copy forwarding, capability-based routing. No cloud middleman. Apache 2.0.",
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
      className={`${jetbrainsMono.variable} ${majorMonoDisplay.variable} ${spaceGrotesk.variable} h-full antialiased`}
    >
      <body className="bg-bg text-ink overflow-x-hidden font-mono min-h-full">
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
