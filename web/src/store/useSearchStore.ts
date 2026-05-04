"use client";

import type { StoreApi, UseBoundStore } from "zustand";
import { create } from "zustand";

export interface SearchState {
  open: boolean;
  setOpen: (open: boolean) => void;
  search: string;
  setSearch: (search: string) => void;
}

export const useSearchStore: UseBoundStore<StoreApi<SearchState>> =
  create<SearchState>((set) => ({
    open: false,
    setOpen: (open: boolean) => {
      set({ open });
    },
    search: "",
    setSearch(search: string) {
      set({ search });
    },
  }));
