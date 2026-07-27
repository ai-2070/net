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

Here is a real one, taken verbatim from the cross-language golden vectors —
a caller that tried to redeem a quote twice:

```json
{
  "object": "net.payment.failure@1",
  "code": "payment",
  "stage": "redeem",
  "reason": "already_redeemed",
  "message": "quote already redeemed — one payment, one serve",
  "retryable": false,
  "recovery": {
    "class": "new_quote_required",
    "actor": "caller_agent",
    "safe_to_retry": false,
    "safe_to_requote": true,
    "next_action": "request_new_quote"
  },
  "handler_executed": false,
  "funds_moved": "yes",
  "prior_payment": "consumed",
  "quote_id": "q_fixture",
  "tool_id": "paid_echo"
}
```

Read it the way an agent would: `safe_to_retry: false` rules out a blind retry,
`safe_to_requote: true` says a fresh quote is legitimate, and `actor:
caller_agent` says no human is needed to unblock it. The money facts are
separate and unambiguous — `funds_moved: "yes"` with `handler_executed: false`
is precisely the case a caller must not paper over by retrying.

The required fields are `object`, `code`, `stage`, `reason`, `message`,
`funds_moved` and `prior_payment` (strings), `retryable` and `handler_executed`
(bools), and `recovery` (an object of `class` / `actor` strings plus
`safe_to_retry` / `safe_to_requote` bools). `quote_id`, `tool_id` and
`recovery.next_action` are optional — but if present they must still be strings,
because a wrong-typed optional fails the predicate too.

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
