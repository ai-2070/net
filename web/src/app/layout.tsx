import type { Metadata } from "next";
import { JetBrains_Mono, Major_Mono_Display } from "next/font/google";
import "./globals.css";

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

export const metadata: Metadata = {
  title: "Net — Network Event Transport. A latency-first encrypted mesh.",
  description:
    "Net is a latency-first encrypted mesh runtime. Every device is a first-class node on a flat, encrypted topology. Nanosecond scheduling, zero-copy forwarding, capability-based routing. ~1.92 MB deployed binary.",
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
      className={`${jetbrainsMono.variable} ${majorMonoDisplay.variable} h-full antialiased`}
    >
      <body className="bg-bg text-ink overflow-x-hidden font-mono min-h-full">
        <TopStatusBar {...repoInfo} />
        {children}
      </body>
    </html>
  );
}
