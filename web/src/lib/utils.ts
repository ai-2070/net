export function formatReleaseDate(iso: string): string {
  if (!iso) return "—";
  return iso.slice(0, 10).replace(/-/g, ".");
}

export function buildQuarter(buildDate: string): string {
  const parts = buildDate.split(".");
  const year = parts[0];
  const monthStr = parts[1];
  if (!year || !monthStr) return buildDate;
  const month = Number.parseInt(monthStr, 10);
  if (Number.isNaN(month) || month < 1 || month > 12) return year;
  return `Q${Math.ceil(month / 3)} ${year}`;
}
