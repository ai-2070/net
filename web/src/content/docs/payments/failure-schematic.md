# The failure schematic

When a payment is refused, a human gets an error message — but an *agent* needs
to know **why**, **who can fix it**, and **what's safe to do next**, without
parsing prose. The `net.payment.failure@1` schematic is that machine-actionable
verdict, riding **beside** the human error, never instead of it.

## What it carries

A denial can attach a `net.payment.failure@1` object with, among other fields:

- **`reason`** — the specific verdict (e.g. `already_redeemed`,
  `insufficient_funds`), snake_case, additive within `@1`;
- **`stage`** — where in the lifecycle it fired (`admission`, `redeem`, …);
- **`recovery`** — the actionable part: a **`class`** (e.g.
  `new_quote_required`), an **`actor`** (who can resolve it —
  `caller_agent` / `caller_user` / operator), and two booleans an agent branches
  on directly: **`safe_to_retry`** and **`safe_to_requote`**;
- **`funds_moved`** / **`prior_payment`** — the money facts (did this refusal
  leave funds moved? was there a prior payment?).

An agent reads `recovery` and acts — request a new quote, top up, back off — instead
of regex-matching an error string.

## It rides beside the human error

The provider sends the ordinary human error body (byte-identical to what the
wire has always carried) and attaches the schematic in a reply header. A
consumer that doesn't understand the schematic still gets the human error; a
consumer that does gets structure too. Producers emit **exactly one** schematic,
as raw JSON bytes; consumers treat a duplicate or malformed header as **absent**
and fall back to the human error — never an error, never a guess.

## Tolerance is a contract

Every language applies the **same tolerant predicate**: decode the header as
strict UTF-8 JSON and accept it **iff** it carries the tag *and* deserializes to
the full schematic shape (required fields present and correctly typed; present
optional fields correctly typed too). A tag-only, mistyped, or structurally
incomplete object is **not** accepted — it falls back to the human error. This is
pinned by cross-language golden vectors so Rust, Python, Node, and Go agree on
exactly which headers are accepted.

## Scope: payments only

`net.payment.failure@1` is for **payment** failures — its `code` is `"payment"`.
Terms, profile, eligibility, and other **non-payment admission failures do not
ride this object.** The schematic's `code` family is designed to generalize
(`policy` / `approval` / `delegation`) but v1 ships only `payment`; a broader
admission-failure vocabulary is future work, not something to shoehorn into the
payment schematic. And nothing here implies Net performs KYB, tax, sanctions,
identity, invoicing, or fulfillment — a refusal reports a payment verdict, not an
eligibility judgment.
