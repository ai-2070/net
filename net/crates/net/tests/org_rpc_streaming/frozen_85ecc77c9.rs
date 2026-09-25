//! The FROZEN old provider at revision `85ecc77c9`, vendored for
//! `frozen_old_provider_refuses_stream_proof_with_not_supported`
//! (`ORG_SCOPED_STREAMING_PLAN.md` §1.4): the `OrgCallProof` type,
//! `verify_org_admission` and `serve_rpc_protected` AS THEY EXISTED at that
//! revision — never the new implementation configured to behave like one.
//!
//! Layout:
//!
//! * [`old_org_call`] — the frozen proof + `"net-org-call-v1"` transcript +
//!   prefix-tolerant decoder (`org_call.rs:50-359` @`85ecc77c9`, verbatim);
//! * [`old_org_admission`] — the frozen `verify_org_admission` with its
//!   `is_unary` step 4 and the frozen denial/coarse types
//!   (`org_admission.rs:64-662`, verbatim);
//! * [`old_serve`] — the frozen `serve_rpc_protected` (`mesh_rpc.rs:3400-3438`,
//!   verbatim) plus the frozen flag derivation (`:1124-1127`) and denial wire
//!   shape (`:872-876`), and the two named shim seams its body needs.
//!
//! Each module's doc lists its extraction command, extraction sha256, and
//! every adaptation line — including [`old_serve`]'s NAMED re-indentation of
//! the denial-shape block (whitespace-insensitive provenance statement +
//! trimmed-hash evidence in that module's doc). See also the runner
//! [`frozen_opening`] in [`old_serve`].

// `#[rustfmt::skip]` on each vendored module: their bodies are byte-identical
// to the recorded `85ecc77c9` extractions (sha256 in each file's doc), and a
// formatting pass would silently destroy that provenance.
#[path = "frozen_85ecc77c9/old_org_admission.rs"]
#[rustfmt::skip]
pub mod old_org_admission;
#[path = "frozen_85ecc77c9/old_org_call.rs"]
#[rustfmt::skip]
pub mod old_org_call;
#[path = "frozen_85ecc77c9/old_serve.rs"]
#[rustfmt::skip]
// Verbatim `85ecc77c9` source: its `std::sync::Mutex::lock` calls are part of
// the frozen extraction this witness exists to pin (sha256 in each file's
// doc). Migrating them to `parking_lot` would falsify the frozen-decoder
// evidence, so the repo-wide disallowed-methods ban is lifted for this module
// only — the vendored file itself stays byte-identical.
#[allow(clippy::disallowed_methods)]
pub mod old_serve;

pub use old_serve::{frozen_opening, FrozenProvider, FrozenYield, UnaryAdmission};

// ===========================================================================
// TESTS-4 — the vendored-body extraction hashes are COMMENT-ONLY no longer.
//
// Every module above records, in its doc, the sha256 of the `85ecc77c9`
// extraction its body was vendored from. The test below RECOMPUTES those
// hashes from the vendored bodies as committed and compares them to the
// recorded values (which it also asserts are still present in the docs), so
// both halves of the provenance claim are executable: the body still hashes
// to the extraction's digest, and the digest in the comment still says so.
//
// Scope, stated plainly: the hash is recomputed over the vendored window with
// LF-normalized line endings (identical to the `git show … | sed -n` digest
// the docs record for a LF checkout) — it does NOT re-run `git show` at
// `85ecc77c9` (a shallow CI checkout may not contain the object). A mutation
// of any vendored body line REDDENS this test; so does an edit of any
// recorded hash in the module docs.
// ===========================================================================

/// One recorded extraction: the vendored file that carries it, where its body
/// sits (anchored on the extraction's first line), how many lines it covers,
/// and the sha256 the module's doc records.
struct Extraction {
    what: &'static str,
    src: &'static str,
    anchor: &'static str,
    lines: usize,
    sha256: &'static str,
}

const OLD_ADMISSION_SRC: &str = include_str!("frozen_85ecc77c9/old_org_admission.rs");
const OLD_CALL_SRC: &str = include_str!("frozen_85ecc77c9/old_org_call.rs");
const OLD_SERVE_SRC: &str = include_str!("frozen_85ecc77c9/old_serve.rs");

/// The four BYTE-IDENTICAL vendored windows (`old_org_call` and
/// `old_org_admission` body-to-EOF; `old_serve`'s `serve_rpc_protected` and
/// `is_unary` derivation). The denial-shape block — re-indented and
/// prefix-retargeted — is covered separately below.
const VERBATIM_WINDOWS: &[Extraction] = &[
    Extraction {
        what: "org_admission.rs:64-662 @85ecc77c9",
        src: OLD_ADMISSION_SRC,
        anchor: "/// The admission mode a provider registered for one capability",
        lines: 599,
        sha256: "ef756fdb76c4ba2eaf7548a437bce8cc897eb60e45955b2cca423b2f6c3fd646",
    },
    Extraction {
        what: "org_call.rs:50-359 @85ecc77c9",
        src: OLD_CALL_SRC,
        anchor: "/// blake3 `derive_key` context for the call-binding transcript",
        lines: 310,
        sha256: "64adefbc5b30b10ba75186572efab97a51aefcd4ce23d8b8541835d1d72f4d2c",
    },
    Extraction {
        what: "mesh_rpc.rs:3400-3438 @85ecc77c9 (serve_rpc_protected)",
        src: OLD_SERVE_SRC,
        anchor: "/// Register a PROTECTED unary RPC handler (E1.1/E1.2). Every call must carry",
        lines: 39,
        sha256: "2b1234f4d377685a39ee0db58218d69c76c8c128fb8bd8b542b4a1a914935292",
    },
    Extraction {
        what: "mesh_rpc.rs:1124-1127 @85ecc77c9 (is_unary derivation)",
        src: OLD_SERVE_SRC,
        anchor: "// Unary only (E1.8): a streaming flag on a protected REQUEST is a distinct",
        lines: 4,
        sha256: "f74300ce94b508da6a439c906f523c4110dd3394d08d5e975e1a28ac3fbc9bf8",
    },
];

/// The denial-shape block (`mesh_rpc.rs:872-876` @`85ecc77c9`): re-indented
/// (+8 columns) and carrying adaptation 2's one prefix retarget, so it is
/// pinned by `old_serve`'s whitespace-insensitive statement instead: trim
/// each line's surrounding whitespace, reverse the one retarget, hash —
/// `f31eb08e…` on both sides. The RAW extraction's own digest (`fa275454…`)
/// is comment-only by construction (the raw bytes are not vendored), so it is
/// bound here to the comment it is recorded in.
const DENIAL_ANCHOR: &str = "let resp = net::adapter::net::cortex::RpcResponsePayload {";
const DENIAL_LINES: usize = 5;
const DENIAL_TRIMMED_SHA256: &str =
    "f31eb08ec127c32f1a4ccf1892ec4dadf0fac151af89aad03cce65b60ad327a7";
const DENIAL_RAW_SHA256: &str = "fa275454b5e7293f7e3090ed0775d1193d520b00cbab9e3f68f5b43deccc5960";

/// The window's lines, located by exact trimmed match on its first line. The
/// anchor must be unique in the vendored file — an ambiguous anchor would let
/// a window silently migrate.
fn window<'a>(src: &'a str, what: &str, anchor: &str, count: usize) -> Vec<&'a str> {
    let lines: Vec<&str> = src.lines().collect();
    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim() == anchor)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "the {what} window anchor {anchor:?} must match exactly one vendored line"
    );
    let start = hits[0];
    assert!(
        start + count <= lines.len(),
        "the {what} window ({count} lines) runs past the end of the vendored file"
    );
    lines[start..start + count].to_vec()
}

/// LF-normalized digest of a window — the byte form `git show | sed -n`
/// produces from the LF repository object (so a CRLF checkout still verifies).
fn lf_joined(lines: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    for line in lines {
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
    }
    buf
}

#[test]
fn every_vendored_body_window_recomputes_to_its_recorded_extraction_sha256() {
    // FIPS 180-4 known-answer vector: the hasher itself is under test too, so
    // a broken SHA-256 cannot silently verify anything.
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );

    for x in VERBATIM_WINDOWS {
        let body = window(x.src, x.what, x.anchor, x.lines);
        assert_eq!(
            sha256_hex(&lf_joined(&body)),
            x.sha256,
            "the vendored body of {} must hash to its recorded extraction sha256 \
             (a changed body line, or a changed recorded hash, breaks this)",
            x.what,
        );
        assert!(
            x.src.contains(x.sha256),
            "the recorded sha256 of {} must still appear in its module's doc",
            x.what,
        );
    }

    // The denial-shape block, via `old_serve`'s whitespace-insensitive
    // statement: trim each line, reverse adaptation 2's one prefix retarget,
    // and the digest must equal the recorded `f31eb08e…`.
    let denial = window(
        OLD_SERVE_SRC,
        "mesh_rpc.rs:872-876 @85ecc77c9 (denial shape)",
        DENIAL_ANCHOR,
        DENIAL_LINES,
    );
    let mut normalized = Vec::new();
    for line in &denial {
        let trimmed = line.trim().replace(
            "net::adapter::net::cortex::RpcResponsePayload",
            "crate::adapter::net::cortex::RpcResponsePayload",
        );
        normalized.push(trimmed);
    }
    let mut buf = Vec::new();
    for line in &normalized {
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
    }
    assert_eq!(
        sha256_hex(&buf),
        DENIAL_TRIMMED_SHA256,
        "the re-indented denial-shape block must stay whitespace-insensitively \
         identical to mesh_rpc.rs:872-876 @85ecc77c9",
    );
    assert!(
        OLD_SERVE_SRC.contains(DENIAL_TRIMMED_SHA256) && OLD_SERVE_SRC.contains(DENIAL_RAW_SHA256),
        "both denial-shape provenance digests (trimmed and raw) must remain in the doc",
    );
}

/// Minimal SHA-256 (FIPS 180-4). Hand-rolled on purpose: the recorded digests
/// are `sha256sum` outputs and no hashing crate rides this test target's
/// dependency graph — five fixed comparisons do not justify one (or its
/// lockfile drift).
fn sha256_hex(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = bytes.to_vec();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let o = 4 * i;
            *word = u32::from_be_bytes([chunk[o], chunk[o + 1], chunk[o + 2], chunk[o + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}
