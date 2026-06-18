#!/usr/bin/env python3
"""arxiv_search — Literature search and full-text fetch tool for Eureka agents.

Reads a JSON call from stdin and writes a JSON result to stdout.

Modes
-----
search  Keyword search via the official arXiv Atom API
        (https://export.arxiv.org/api/query).  Free, no key required,
        relevance-ranked, ~2.4 M papers.

fetch   Retrieve a paper's full text as clean Markdown via the arxiv2md REST
        API (https://arxiv2md.org).  No package install required.

Input schema
------------
{
  "mode": "search" | "fetch",
  "query": "...",        # required for mode=search
  "arxiv_id": "...",     # required for mode=fetch  (e.g. "2404.01234")
  "max_results": 5,      # optional, mode=search only, default 5
  "page": 0              # optional, zero-based page, default 0
}

Dependencies
------------
  stdlib only (xml.etree.ElementTree + urllib)
"""

import json
import sys
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

ARXIV_API    = "https://export.arxiv.org/api/query"
ARXIV2MD_API = "https://arxiv2md.org/api/markdown"
MAX_FULL_TEXT = 50_000  # chars; keeps context window manageable

# XML namespaces used by the Atom feed
_NS = {
    "atom":       "http://www.w3.org/2005/Atom",
    "opensearch": "http://a9.com/-/spec/opensearch/1.1/",
    "arxiv":      "http://arxiv.org/schemas/atom",
}


# ---------------------------------------------------------------------------
# Search — official arXiv Atom API
# ---------------------------------------------------------------------------

def search_arxiv(query: str, max_results: int, page: int = 0) -> dict:
    """Relevance-ranked keyword search via the arXiv Atom API."""
    params = urllib.parse.urlencode({
        "search_query": f"all:{query}",
        "start":        page * max_results,
        "max_results":  max_results,
        "sortBy":       "relevance",
        "sortOrder":    "descending",
    })
    url = f"{ARXIV_API}?{params}"
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "eureka-agent/1.0"})
        with urllib.request.urlopen(req, timeout=30) as resp:
            xml_text = resp.read().decode()
    except Exception as exc:  # noqa: BLE001
        return {"error": f"arXiv API request failed: {exc}", "papers": []}

    try:
        root = ET.fromstring(xml_text)
    except ET.ParseError as exc:
        return {"error": f"Failed to parse arXiv response: {exc}", "papers": []}

    total_str = root.findtext("opensearch:totalResults", namespaces=_NS) or "0"
    try:
        total = int(total_str)
    except ValueError:
        total = 0

    papers = []
    for entry in root.findall("atom:entry", _NS):
        raw_id   = entry.findtext("atom:id", namespaces=_NS) or ""
        arxiv_id = raw_id.split("/abs/")[-1].strip()

        title    = (entry.findtext("atom:title",   namespaces=_NS) or "").replace("\n", " ").strip()
        abstract = (entry.findtext("atom:summary", namespaces=_NS) or "").replace("\n", " ").strip()
        published = (entry.findtext("atom:published", namespaces=_NS) or "")[:4]

        author_els  = entry.findall("atom:author", _NS)
        author_names = [
            (a.findtext("atom:name", namespaces=_NS) or "")
            for a in author_els[:5]
        ]
        authors = ", ".join(author_names) + (" et al." if len(author_els) > 5 else "")

        categories = " ".join(
            cat.get("term", "") for cat in entry.findall("atom:category", _NS)
        )

        doi_el = entry.find("arxiv:doi", _NS)
        doi    = doi_el.text.strip() if doi_el is not None and doi_el.text else ""

        papers.append({
            "arxiv_id":   arxiv_id,
            "title":      title,
            "abstract":   abstract,
            "authors":    authors,
            "categories": categories,
            "year":       published,
            "doi":        doi,
        })

    start = page * max_results
    return {
        "papers":      papers,
        "total_found": total,
        "page":        page,
        "max_results": max_results,
        "has_more":    start + max_results < total,
    }


# ---------------------------------------------------------------------------
# Fetch
# ---------------------------------------------------------------------------

def fetch_paper(arxiv_id: str) -> dict:
    """Fetch a paper as clean Markdown via the arxiv2md REST API.

    No package install required — single GET to arxiv2md.org.
    Rate limit: 30 req/min per IP.
    """
    paper_id = arxiv_id.strip()
    params   = urllib.parse.urlencode({
        "url":               paper_id,
        "remove_refs":       "true",
        "remove_toc":        "true",
        "remove_citations":  "true",
    })
    url = f"{ARXIV2MD_API}?{params}"
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "eureka-agent/1.0"})
        with urllib.request.urlopen(req, timeout=60) as resp:
            text = resp.read().decode()
    except Exception as exc:  # noqa: BLE001
        return {"error": f"arxiv2md fetch failed for '{paper_id}': {exc}"}

    if not text.strip():
        return {"error": f"arxiv2md returned empty content for '{paper_id}' (paper may predate HTML conversion)"}

    return {
        "arxiv_id":  paper_id,
        "text":      text[:MAX_FULL_TEXT],
        "truncated": len(text) > MAX_FULL_TEXT,
    }


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    try:
        args = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        args = {}

    # Infer mode from fields when not explicit — models often call the tool by
    # a logical name (arxiv_search vs arxiv_fetch) and omit the mode field.
    if "mode" in args:
        mode = args["mode"]
    elif "arxiv_id" in args and args.get("arxiv_id"):
        mode = "fetch"
    else:
        mode = "search"

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
