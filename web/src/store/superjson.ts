"use client";

import superjson from "superjson";
import type { PersistStorage, StorageValue } from "zustand/middleware";

/**
 * Gets an item from localStorage and deserializes it using superjson
 */
export const getItem = <T>(name: string): StorageValue<T> | null => {
  const str = typeof window === "undefined" ? null : localStorage.getItem(name);
  if (!str) return null;

  try {
    // Use more specific type assertions
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const parsed = JSON.parse(str) as {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      state: any;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      meta: any;
    };

    // eslint-disable-next-line @typescript-eslint/no-unsafe-assignment, @typescript-eslint/no-explicit-any
    return superjson.deserialize({
      // eslint-disable-next-line @typescript-eslint/no-unsafe-assignment, @typescript-eslint/no-explicit-any
      json: parsed.state,
      // eslint-disable-next-line @typescript-eslint/no-unsafe-assignment, @typescript-eslint/no-explicit-any
      meta: parsed.meta,
    });
  } catch (error) {
    console.error("Error deserializing store:", error);
    return null;
  }
};

/**
 * Serializes a value using superjson and stores it in localStorage
 */
export const setItem = <T>(name: string, value: StorageValue<T>): void => {
  if (typeof window !== "undefined") {
    // eslint-disable-next-line @typescript-eslint/no-unsafe-assignment
    const serialized = superjson.serialize(value);
    const str = JSON.stringify({
      state: serialized.json,
      meta: serialized.meta,
    });
    localStorage.setItem(name, str);
  }
};

/**
 * Removes an item from localStorage
 */
export const removeItem = (name: string): void => {
  if (typeof window !== "undefined") {
    localStorage.removeItem(name);
  }
};

/**
 * A storage object compatible with Zustand's persist middleware
 * that uses superjson for serialization/deserialization
 */
export const superjsonStorage = {
  getItem,
  setItem,
  removeItem,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
} as PersistStorage<any>;
