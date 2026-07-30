"""The docs page set, computed once for every checker that needs it.

Two checkers ask "is this a real docs page": the tier manifest (every page has a
state) and the capability records (an absence `alternative.href` points
somewhere). They must agree, and the answer depends on one non-obvious rule —
a folder README renders at the folder's URL, so `start/README.md` is the page
`start`, matching `web/src/lib/docs.ts` rather than the filesystem. Two copies of
that rule would eventually disagree about exactly the pages that are hardest to
notice.
"""

from __future__ import annotations

import os

DEFAULT_DOCS = "web/src/content/docs"


SHARED_BODY = "_shared.md"


def page_slugs(docs_dir: str) -> set[str]:
    """Every page, keyed by the slug its URL uses.

    An adaptive page is ONE page with internal structure, not five. A directory
    holding `_shared.md` is the page; its per-lens fragments are parts of it and
    are not separately routable, which mirrors `lib/docs.ts` pulling them out of
    `children`. Counting them as pages would inflate the corpus by four per
    conversion and make the migration ratchet report progress backwards.
    """
    out: set[str] = set()
    for dirpath, _dirs, files in os.walk(docs_dir):
        if SHARED_BODY in files:
            rel = os.path.relpath(dirpath, docs_dir)
            out.add(rel.replace(os.sep, "/"))
            _dirs.clear()  # nothing below an adaptive page is a page
            continue
        rel_dir = os.path.relpath(dirpath, docs_dir)
        # Dot directories (`.next` lives under the content tree), tested per path
        # COMPONENT. A substring test for `os.sep + "."` also matches the `/..` in
        # a relative path and silently prunes the whole walk.
        if any(part.startswith(".") and part not in (".", "..")
               for part in rel_dir.split(os.sep)):
            continue
        for name in files:
            if not name.endswith(".md"):
                continue
            rel = os.path.relpath(os.path.join(dirpath, name), docs_dir)
            parts = rel[:-3].split(os.sep)
            if parts[-1] == "README":
                parts = parts[:-1]
            out.add("/".join(parts) or "index")
    return out
