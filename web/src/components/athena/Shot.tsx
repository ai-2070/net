"use client";

import Image from "next/image";
import { useRef } from "react";

export interface ShotProps {
  src: string;
  alt: string;
  caption: string;
  tag?: string;
  width?: number;
  height?: number;
  sizes?: string;
}

// A dashboard screenshot in a slab. Clicking opens it full size; Esc, the
// close button or a click anywhere closes it.
export function Shot({
  src,
  alt,
  caption,
  tag,
  width = 1600,
  height = 1000,
  sizes = "(min-width: 1440px) 900px, (min-width: 1024px) 60vw, 100vw",
}: ShotProps) {
  const dialogRef = useRef<HTMLDialogElement | null>(null);
  const close = (): void => dialogRef.current?.close();

  return (
    <>
      <figure className="slab m-0 overflow-hidden">
        <button
          type="button"
          onClick={() => dialogRef.current?.showModal()}
          aria-label={`Enlarge: ${caption}`}
          className="group relative block w-full p-0 border-0 bg-transparent cursor-zoom-in"
        >
          <Image
            src={src}
            alt={alt}
            width={width}
            height={height}
            sizes={sizes}
            className="w-full h-auto transition-opacity group-hover:opacity-90"
          />
          <span className="absolute bottom-3 right-3 text-[12px] tracking-[0.14em] uppercase text-ink bg-bg/80 border border-line px-2 py-1 opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100">
            ⤢ enlarge
          </span>
        </button>
        <figcaption className="flex items-center justify-between gap-3 border-t border-line px-4 py-2.5 text-[12px] tracking-[0.14em] uppercase text-ink-dim">
          <span>{caption}</span>
          {tag ? (
            <span className="text-accent font-mono normal-case tracking-normal">
              {tag}
            </span>
          ) : null}
        </figcaption>
      </figure>

      <dialog
        ref={dialogRef}
        onClick={close}
        aria-label={caption}
        className="m-auto p-0 border border-line bg-bg max-w-[96vw] max-h-[94vh] overflow-hidden backdrop:bg-black/85 cursor-zoom-out"
      >
        <figure className="m-0">
          <Image
            src={src}
            alt={alt}
            width={width}
            height={height}
            unoptimized
            className="block w-auto h-auto max-w-[96vw] max-h-[calc(94vh-42px)]"
          />
          <figcaption className="flex items-center justify-between gap-3 border-t border-line px-4 py-2.5 text-[12px] tracking-[0.14em] uppercase text-ink-dim">
            <span>{caption}</span>
            <button
              type="button"
              onClick={close}
              className="text-ink hover:text-accent cursor-pointer bg-transparent border-0 p-0 text-[12px] tracking-[0.14em] uppercase"
            >
              close ✕
            </button>
          </figcaption>
        </figure>
      </dialog>
    </>
  );
}
