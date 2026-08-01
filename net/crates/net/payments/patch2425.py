import pathlib

ROOT = pathlib.Path('../../..')  # repo root from payments/

# --- 24a: quote_request imports the registry tag ---------------------------
p = pathlib.Path('src/core/quote_request.rs')
s = p.read_text(encoding='utf-8')
old = '''use super::canonical::{EnvelopeError, ExtraFields, SignatureHex, SignedEnvelope};
use super::versioning::ensure_tag;

/// `net.payment.quote_request@1`.
pub const TAG_QUOTE_REQUEST: &str = "net.payment.quote_request@1";
'''
new = '''use super::canonical::{EnvelopeError, ExtraFields, SignatureHex, SignedEnvelope};
use super::versioning::ensure_tag;
// The tag lives in the versioning registry with every other envelope's, so
// the wire string has exactly one definition. Re-exported here because
// this is where readers of the envelope look for it.
pub use super::versioning::TAG_QUOTE_REQUEST;
'''
assert old in s, 'quote_request tag'
p.write_text(s.replace(old, new, 1), encoding='utf-8')
print('24a: tag deduped')

# --- 24b: binding_required mapping row -------------------------------------
p = pathlib.Path('../sdk/src/tool_payment.rs')
s = p.read_text(encoding='utf-8')
old = '''/// | `binding_malformed` | redeem | caller_configuration_error | caller_operator | false | false | false | unknown | unknown | `fix_payment_client` |'''
new = '''/// | `binding_malformed` | redeem | caller_configuration_error | caller_operator | false | false | false | unknown | unknown | `fix_payment_client` |
/// | `binding_required` | redeem | caller_configuration_error | caller_operator | false | false | false | unknown | unknown | `fix_payment_client` |'''
assert old in s, 'mapping table'
p.write_text(s.replace(old, new, 1), encoding='utf-8')
print('24b: mapping row added')

# --- 25b/c: rpc.rs doc corrections -----------------------------------------
p = ROOT / 'net/crates/net/src/adapter/net/cortex/rpc.rs'
s = p.read_text(encoding='utf-8')
old = '''    ///   originator (see `RpcInboundEvent::session_node`);'''
new = '''    ///   originator (it is `RpcInboundEvent::from_node`, the wire-session
    ///   peer's `NodeId`);'''
assert old in s, 'session_node reference'
s = s.replace(old, new, 1)

old = '''    /// AEAD-verified caller `origin_hash`. Same source as
    /// [`RpcContext::caller_origin`].
    pub caller_origin: u64,'''
new = '''    /// Caller's `origin_hash`, from the inbound packet header. Same
    /// source, and the same caveat, as [`RpcContext::caller_origin`]:
    /// **routing metadata, not identity authentication — do not
    /// authorize on this.**
    pub caller_origin: u64,'''
assert old in s, 'streaming caller_origin'
s = s.replace(old, new, 1)
p.write_text(s, encoding='utf-8')
print('25b/c: rpc docs corrected')

# --- 25d: spend.rs preimage claim -----------------------------------------
p = pathlib.Path('src/policy/spend.rs')
s = p.read_text(encoding='utf-8')
old = '''    /// that state requires a BLAKE3 preimage over the whole quote
    /// transcript, so this is API integrity rather than a live attack
    /// path — but "approve" should not be able to invent the thing it
    /// approves.'''
new = '''    /// that state requires a later quote to be issued with exactly that
    /// id, and quote ids are content-derived — so it is a quote-id
    /// collision, not something an attacker steers. This is API
    /// integrity rather than a live attack path; "approve" simply should
    /// not be able to invent the thing it approves.'''
assert old in s, 'preimage claim'
p.write_text(s.replace(old, new, 1), encoding='utf-8')
print('25d: preimage claim corrected')

# --- 25a: audit doc, three sites -> four -----------------------------------
p = ROOT / 'docs/internal/misc/SECURITY_AUDIT_2026_08_01_PAYMENTS.md'
s = p.read_text(encoding='utf-8')
old = '''**Scope, stated precisely.** All three sites are **caller-side** — the payer logging its own quote id.'''
new = '''**Scope, stated precisely.** All four sites — `flow/mod.rs:754` and `:761` in proof building, `flow/mod.rs:929`, and `flow/http402.rs:446` — are **caller-side**: the payer logging its own quote id.'''
assert old in s, 'audit M3 scope'
s = s.replace(old, new, 1)
p.write_text(s, encoding='utf-8')
print('25a: audit site count corrected')
