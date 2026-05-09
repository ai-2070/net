import { twMerge } from "tailwind-merge";

export type ClassValue = string | false | null | undefined;

export function cn(...classes: ReadonlyArray<ClassValue>): string {
  return twMerge(
    classes.filter((c): c is string => typeof c === "string").join(" "),
  );
}
