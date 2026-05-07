import type { Metadata } from "next";
import { JetBrains_Mono, Major_Mono_Display } from "next/font/google";
import "./globals.css";

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

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html
      lang="en"
      className={`${jetbrainsMono.variable} ${majorMonoDisplay.variable} h-full antialiased`}
    >
      <body className="crt-scanlines bg-bg text-ink overflow-x-hidden font-mono min-h-full flex flex-col">
        {children}
      </body>
    </html>
  );
}
