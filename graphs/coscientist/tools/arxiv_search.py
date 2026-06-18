#!/usr/bin/env python3
"""arxiv_search — Literature search and full-text fetch tool for Eureka agents.

Reads a JSON call from stdin and writes a JSON result to stdout.

Modes
-----
search  Search arXiv metadata via the HuggingFace dataset snapshot.
        Returns paper metadata (title, abstract, authors, year, arxiv_id).

fetch   Fetch and convert a specific arXiv paper to markdown via arxiv2md.
        Returns the paper text (truncated to ~50 k chars if large).

Input schema
------------
{
  "mode": "search" | "fetch",
  "query": "...",        # required for mode=search
  "arxiv_id": "...",     # required for mode=fetch  (e.g. "2404.01234")
  "max_results": 5       # optional, mode=search only, default 5
}

Dependencies
------------
  pip install arxiv2md          # for mode=fetch full text
  (search mode has no deps beyond stdlib)
"""

import json
import sys
import urllib.parse
import urllib.request

HF_SEARCH_URL = "https://datasets-server.huggingface.co/search"
DATASET = "librarian-bots/arxiv-metadata-snapshot"
MAX_FULL_TEXT = 50_000  # chars; keeps context window manageable


# ---------------------------------------------------------------------------
# Search
# ---------------------------------------------------------------------------

def search_arxiv(query: str, max_results: int, page: int) -> dict:
    """Search arXiv metadata via the HuggingFace datasets server REST API.

    No local data download required — the API does the filtering server-side.
    """
    params = urllib.parse.urlencode({
        "dataset": DATASET,
        "config": "default",
        "split": "train",
        "query": query,
        "offset": page * max_results,
        "length": max_results,
    })
    url = f"{HF_SEARCH_URL}?{params}"
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "eureka-agent/1.0"})
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read().decode())
    except Exception as exc:  # noqa: BLE001
        return {"error": f"HF dataset search failed: {exc}", "papers": []}

    papers = []
    for row_obj in data.get("rows", []):
        row = row_obj.get("row", {})
        authors_raw = row.get("authors", "")
        if isinstance(authors_raw, list):
            short = [str(a) for a in authors_raw[:5]]
            authors = ", ".join(short) + (" et al." if len(authors_raw) > 5 else "")
        else:
            authors = str(authors_raw)

        papers.append({
            "arxiv_id":  row.get("id", "").strip(),
            "title":     row.get("title", "").strip(),
            "abstract":  row.get("abstract", "").strip(),
            "authors":   authors,
            "categories": row.get("categories", ""),
            "year":      str(row.get("update_date", ""))[:4],
            "doi":       row.get("doi", "") or "",
        })

    total = data.get("num_rows_total", len(papers))
    return {
        "papers": papers,
        "total_found": total,
        "page": page,
        "max_results": max_results,
        "has_more": (page + 1) * max_results < total,
    }


# ---------------------------------------------------------------------------
# Fetch
# ---------------------------------------------------------------------------

def fetch_paper(arxiv_id: str) -> dict:
    """Fetch and convert an arXiv paper to markdown via arxiv2md.

    Falls back to returning the abstract from the metadata snapshot if the
    arxiv2md package is not installed or conversion fails.
    """
    paper_id = arxiv_id.strip()

    # --- attempt 1: arxiv2md.arxiv_to_md ---------------------------------
    try:
        from arxiv2md import arxiv_to_md  # type: ignore[import-untyped]
        text = arxiv_to_md(paper_id)
        if isinstance(text, str) and text.strip():
            return {
                "arxiv_id": paper_id,
                "text": text[:MAX_FULL_TEXT],
                "truncated": len(text) > MAX_FULL_TEXT,
            }
    except (ImportError, Exception):  # noqa: BLE001
        pass

    # --- attempt 2: arxiv2md.convert (alternate API shape) ---------------
    try:
        import arxiv2md  # type: ignore[import-untyped]
        fn = getattr(arxiv2md, "convert", None) or getattr(arxiv2md, "fetch", None)
        if callable(fn):
            text = fn(paper_id)
            if isinstance(text, str) and text.strip():
                return {
                    "arxiv_id": paper_id,
                    "text": text[:MAX_FULL_TEXT],
                    "truncated": len(text) > MAX_FULL_TEXT,
                }
    except (ImportError, Exception):  # noqa: BLE001
        pass

    # --- fallback: return abstract from metadata snapshot -----------------
    result = search_arxiv(paper_id, 1)
    papers = result.get("papers", [])
    if papers:
        p = papers[0]
        text = (
            f"# {p['title']}\n\n"
            f"**Authors**: {p['authors']}\n"
            f"**Year**: {p['year']}  "
            f"**arXiv**: {p['arxiv_id']}\n\n"
            f"## Abstract\n\n{p['abstract']}"
        )
        return {
            "arxiv_id": paper_id,
            "text": text,
            "truncated": False,
            "note": "arxiv2md not installed — abstract only. Run: pip install arxiv2md",
        }

    return {"error": f"Could not fetch '{paper_id}': arxiv2md not installed and paper not found in metadata snapshot."}


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    try:
        args = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        args = {}

    mode = args.get("mode", "search")

    if mode == "search":
        query = args.get("query", "").strip()
        if not query:
            result: dict = {"error": "query is required for mode=search"}
        else:
            max_results = max(1, min(int(args.get("max_results", 5)), 20))
            page = max(0, int(args.get("page", 0)))
            result = search_arxiv(query, max_results, page)

    elif mode == "fetch":
        arxiv_id = args.get("arxiv_id", "").strip()
        if not arxiv_id:
            result = {"error": "arxiv_id is required for mode=fetch"}
        else:
            result = fetch_paper(arxiv_id)

    else:
        result = {"error": f"Unknown mode '{mode}'. Use 'search' or 'fetch'."}

    print(json.dumps(result))


if __name__ == "__main__":
    main()
