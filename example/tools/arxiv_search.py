#!/usr/bin/env python3
"""arxiv_search — Literature search and paper explorer for Eureka agents.

Reads a JSON call from stdin and writes a JSON result to stdout.

Modes
-----
search   Keyword search via the official arXiv Atom API
         (https://export.arxiv.org/api/query).  Free, no key required,
         relevance-ranked, ~2.4 M papers.

fetch    Retrieve a paper via the arxiv2md REST API
         (https://arxiv2md.org).  Two sub-modes:

         * Overview (default): returns title + table of contents with
           per-section char counts and preview snippets.  Token-efficient;
           the agent can then request specific sections.

         * Section fetch: when `sections` is provided, returns the full
           text of only the requested sections (by index or heading name).

Input schema
------------
{
  "mode":        "search" | "fetch",
  "query":       "...",     # required for mode=search
  "arxiv_id":    "...",     # required for mode=fetch  (e.g. "2404.01234")
  "max_results": 5,         # optional, mode=search only, default 5
  "page":        0,         # optional, zero-based page, default 0
  "sections":    ["0","Intro"]  # optional, mode=fetch only
}

Output — search
---------------
{
  "papers":      [ { arxiv_id, title, abstract, authors, categories, year, doi } ],
  "total_found": int,
  "page":        int,
  "max_results": int,
  "has_more":    bool
}

Output — fetch (overview, no `sections`)
----------------------------------------
{
  "arxiv_id":  "...",
  "title":     "Paper Title",
  "sections":  [
    { "index": 0, "heading": "Abstract", "level": 2,
      "chars": 1200, "preview": "First ~200 chars..." },
    ...
  ],
  "total_chars":    35000,
  "total_sections": 8,
  "truncated":      false,
  "hint":           "Call again with sections=[index_or_heading...] ..."
}

Output — fetch (with `sections`)
---------------------------------
{
  "arxiv_id":   "...",
  "title":      "Paper Title",
  "sections":   [
    { "heading": "Introduction", "text": "Full text..." }
  ],
  "retrieved":  1,
  "available":  8,
  "truncated":  false
}

Dependencies
------------
  stdlib only (re, xml.etree.ElementTree + urllib)
"""

import json
import re
import sys
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

ARXIV_API    = "https://export.arxiv.org/api/query"
ARXIV2MD_API = "https://arxiv2md.org/api/markdown"
MAX_FULL_TEXT = 50_000  # chars; keeps context window manageable

_NS = {
    "atom":       "http://www.w3.org/2005/Atom",
    "opensearch": "http://a9.com/-/spec/opensearch/1.1/",
    "arxiv":      "http://arxiv.org/schemas/atom",
}

_HEADING_RE = re.compile(r"^(#{1,6})\s+(.+)")


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
# Markdown section parser
# ---------------------------------------------------------------------------

def _parse_sections(text: str) -> tuple[str, list[dict]]:
    """Parse markdown into a title and a flat list of headed sections.

    Returns (title, sections) where each section is:
        { "heading": str, "level": int, "content": str, "chars": int }
    The first ``#``-level heading is treated as the paper title.
    Content before the first heading is discarded.
    """
    lines = text.split("\n")
    sections: list[dict] = []
    title = ""
    current_heading: str | None = None
    current_level = 0
    current_lines: list[str] = []

    for line in lines:
        m = _HEADING_RE.match(line.strip())
        if m:
            level = len(m.group(1))
            heading_text = m.group(2)

            if level == 1 and not title:
                title = heading_text
                continue

            if current_heading is not None:
                content = "\n".join(current_lines).strip()
                if content:
                    sections.append({
                        "heading": current_heading,
                        "level":   current_level,
                        "content": content,
                        "chars":   len(content),
                    })

            current_heading = heading_text
            current_level = level
            current_lines = []
        elif current_heading is not None:
            current_lines.append(line)

    if current_heading is not None:
        content = "\n".join(current_lines).strip()
        if content:
            sections.append({
                "heading": current_heading,
                "level":   current_level,
                "content": content,
                "chars":   len(content),
            })

    return title, sections


def _match_section(filt: str, sections: list[dict]) -> list[int]:
    """Match a filter string against section indices and headings.

    If *filt* is a non-negative integer string it is treated as a 0-based
    index.  Otherwise it is treated as a case-insensitive heading substring
    search.  Returns a list of matching 0-based indices (may be empty).
    """
    try:
        idx = int(filt)
        if 0 <= idx < len(sections):
            return [idx]
        return []
    except ValueError:
        pass

    indices = []
    for i, s in enumerate(sections):
        if filt.lower() in s["heading"].lower():
            indices.append(i)
    return indices


# ---------------------------------------------------------------------------
# Fetch — arxiv2md with token‑efficient overview / section drill‑down
# ---------------------------------------------------------------------------

def fetch_paper(arxiv_id: str, sections_filter: list | None = None) -> dict:
    """Retrieve a paper via arxiv2md and return either an overview or
    specific sections.

    *sections_filter* — if ``None`` (default), returns an overview:
      title + per‑section TOC with char counts and preview snippets.
      If a list of strings, returns full text for only the matched
      sections.
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

    truncated = len(text) > MAX_FULL_TEXT
    text = text[:MAX_FULL_TEXT]

    title, sections = _parse_sections(text)
    result: dict = {
        "arxiv_id":  paper_id,
        "title":     title,
        "truncated": truncated,
    }

    if sections_filter is not None:
        indices: list[int] = []
        for filt in sections_filter:
            indices.extend(_match_section(str(filt), sections))

        if not indices:
            headings = [s["heading"][:60] for s in sections]
            result["sections"] = []
            result["retrieved"] = 0
            result["available"] = len(sections)
            result["available_headings"] = headings
            return result

        matched = []
        for idx in sorted(set(indices)):
            s = sections[idx]
            matched.append({
                "heading": s["heading"],
                "text":    s["content"],
            })
        result["sections"] = matched
        result["retrieved"] = len(matched)
        result["available"] = len(sections)
    else:
        overview = []
        for i, s in enumerate(sections):
            plain = re.sub(r"\s+", " ", s["content"]).strip()
            preview = plain[:200]
            if len(plain) > 200:
                preview += "…"
            overview.append({
                "index":   i,
                "heading": s["heading"],
                "level":   s["level"],
                "chars":   s["chars"],
                "preview": preview,
            })
        result["sections"] = overview
        result["total_sections"] = len(sections)
        result["total_chars"] = sum(s["chars"] for s in sections)
        result["hint"] = (
            "Call again with sections=[index_or_heading...] "
            "to retrieve full text of specific sections."
        )

    return result


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    try:
        args = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        args = {}

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
            sections_filter = args.get("sections")
            result = fetch_paper(arxiv_id, sections_filter)

    else:
        result = {"error": f"Unknown mode '{mode}'. Use 'search' or 'fetch'."}

    print(json.dumps(result))


if __name__ == "__main__":
    main()
