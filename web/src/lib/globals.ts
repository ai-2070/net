/* eslint-disable unicorn/numeric-separators-style */
const globals = {
  isVercel: Boolean(process.env.NEXT_PUBLIC_VERCEL_URL),
  isProduction: Boolean(process.env.NEXT_PUBLIC_VERCEL_URL),
  env: Boolean(process.env.RAILWAY_DEPLOYMENT_ID)
    ? "production"
    : ((process.env.NEXT_PUBLIC_VERCEL_ENV ?? "development") as
        | "production"
        | "preview"
        | "development"),
  isClient: typeof window !== "undefined",
  defaultTtl: 2 * 60 * 1000, // 2 minutes,
  email: "makerseven7@gmail.com",
};

export default globals;
