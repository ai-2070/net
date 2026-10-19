"use client";

import { FieldLabel as SectionLabel } from "./field/FieldLabel";
import { FieldHeading as DisplayHeading } from "./field/FieldHeading";
import { Lede } from "./field/Lede";
import { IsoDiagram, type IsoFigure } from "./field/IsoDiagram";
import { DatafortsConsole } from "./DatafortsConsole";

const CAPABILITY_STRIP: ReadonlyArray<{
  num: string;
  name: string;
  body: string;
  isNew?: boolean;
}> = [
  {
    num: "mesh.storage.1",
    name: "Overflow",
    isNew: true,
    body: "storage doesn't run out. when one disk fills up, the mesh catches the spillover.",
  },
  {
    num: "mesh.storage.2",
    name: "Data Gravity",
    body: "the files aren't moved. files settle near nodes that use them.",
  },
  {
    num: "mesh.storage.3",
    name: "Content-Addressed",
    body: "the hash is the handle. one address gets you the file — wherever it lives.",
  },
  {
    num: "mesh.storage.4",
    name: "Durability Tiers",
    body: "pick your guarantee. fire-and-forget, fsync, or N-peer replicated. one call.",
  },
];

const STORAGE_FIGURES: ReadonlyArray<IsoFigure> = [
  "overflow",
  "gravity",
  "content-addressed",
  "durability",
];

export function DatafortsSection() {
  return (
    <section
      id="dataforts"
      className="relative overflow-hidden px-6 lg:px-12 py-24 lg:py-32"
    >
      <SectionLabel>§07 / storage // new</SectionLabel>
      <DisplayHeading>
        Dataforts:
        <br />
        <span className="text-accent">
          data became
          <br />a fluid.
        </span>
      </DisplayHeading>

      <Lede>
        For 60 years, files were objects nailed to a location — a disk in a box.
        Traditional storage treats files like permanent objects locked to a
        single machine.
        <br />
        <br />
        <strong className="text-accent font-medium">
          Dataforts treats storage as flow and data as fluid.
        </strong>{" "}
        When a device approaches capacity, it overflows onto the mesh. The
        capacity is the mesh. The folder stays local. Reads create gravity. Hot
        data moves closer. Everything is in motion.
      </Lede>

      <DatafortsConsole />

      {/* Capability strip — compact horizontal list */}
      <div className="mt-20 grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-x-10 gap-y-10">
        {CAPABILITY_STRIP.map((c, i) => (
          <div key={c.name} className="border-t border-line pt-5">
            {STORAGE_FIGURES[i] ? (
              <IsoDiagram figure={STORAGE_FIGURES[i]} className="mb-5" />
            ) : null}
            <h3 className="font-display font-bold text-[20px] leading-[1.1] text-ink mb-2 tracking-[-0.01em] flex items-center gap-3">
              {c.name}
              {c.isNew ? (
                <span className="bg-accent text-bg px-1.5 py-0.5 font-mono text-[12px] font-bold tracking-[0.18em]">
                  NEW
                </span>
              ) : null}
            </h3>
            <p className="text-[16px] text-ink-dim leading-[1.55]">{c.body}</p>
            <div className="stencil mt-4 normal-case">▸ {c.num}</div>
          </div>
        ))}
      </div>
    </section>
  );
}
