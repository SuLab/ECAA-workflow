#!/usr/bin/env python3
"""Agent-side literature retrieval + snapshot + manifest helper.

Invoked by the execution agent for an atom that carries
`attributes.retrieval_tools` (e.g. `survey_method_landscape`). Performs
source-class-bounded retrieval against the index APIs for the enabled
source classes, snapshots every fetched source to
`<out_dir>/evidence/snapshots/<sha256>`, writes/extends
`<out_dir>/evidence/manifest.json` (FOUNDATION manifest schema_version 2),
and appends rows to `<out_dir>/method_landscape.csv`.

Source classes and their index endpoints:
  - primary_literature    -> NCBI E-utilities (eutils.ncbi.nlm.nih.gov)
  - conference_proceedings -> OpenAlex (api.openalex.org) / Crossref
                              (api.crossref.org)
  - tool_documentation     -> allowlisted doc domains (readthedocs.io,
                              github.io, bioconductor.org, ...)

The network layer is two monkeypatchable functions, `_http_get_json` and
`_http_get_text`, each of which asserts the target host is on the caller's
allowlist BEFORE any fetch (bounded egress at the helper level, in addition
to the atom's `safety.network` allowlist (enforced by the harness only on
SLURM/AWS executors — NOT on the local executor, where egress is unrestricted;
the helper-level allowlist below is the only egress bound in local runs)).

Pure standard library: urllib, hashlib, json, csv, re. No pip installs.
"""

from __future__ import annotations

import csv
import fcntl
import hashlib
import json
import os
import re
import sys
import time as _time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple
from urllib.error import HTTPError
from urllib.parse import urlparse
from urllib.request import Request, urlopen

# --------------------------------------------------------------------------
# Constants conforming to the FOUNDATION manifest v2 contract.
# --------------------------------------------------------------------------

MANIFEST_SCHEMA_VERSION = 2
EXTRACTED_TEXT_NORMALIZATION = "collapse_whitespace_lowercase_v1"
USER_AGENT = "ecaa-workflow-literature-fetch/1 (+https://github.com/SuLab/ECAA-workflow)"
HTTP_TIMEOUT_SECS = 30
RETRIEVAL_SCOPE_SCHEMA_VERSION = 1

# NCBI E-utilities rate limits: 3 req/s per IP without an API key, 10 req/s
# with ECAA_LIT_NCBI_API_KEY. The helper bursts esearch + N efetch per axis, so
# without pacing it trips HTTP 429 instantly and degrades every axis to the
# curated fallback (the real in-container retrieval blocker). Pace to stay just
# under the limit and retry 429/503 with exponential backoff (honoring
# Retry-After). Module-level so tests can zero the interval.
_MIN_REQUEST_INTERVAL = 0.12 if os.environ.get("ECAA_LIT_NCBI_API_KEY", "").strip() else 0.35
_HTTP_MAX_RETRIES = 4
_HTTP_MAX_BACKOFF_SECS = 8.0
_last_request_monotonic = [0.0]

# CSV column order for method_landscape.csv (version_context optional, last).
CSV_COLUMNS = [
    "axis",
    "candidate_method",
    "source_ref_kind",
    "source_ref",
    "source_class",
    "evidence_role",
    "evidence_quote",
    "evidence_quote_offset",
    "source_kind",
    "source_hash",
    "retrieval_ts",
    "redistributable",
    "verified",
    "version_context",
    # Singular PMID locator for primary-literature rows. The harness
    # claims-matrix validators read this column directly; empty for
    # non-PMID (DOI / URL / curated_baseline) rows.
    "pmid",
]

# Default index hosts per source class when a route lacks explicit hosts.
DEFAULT_ROUTES: Dict[str, Dict[str, Any]] = {
    "primary_literature": {"hosts": ["eutils.ncbi.nlm.nih.gov", "ftp.ncbi.nlm.nih.gov"]},
    "conference_proceedings": {"hosts": ["api.openalex.org", "api.crossref.org"]},
    "tool_documentation": {
        "hosts": [
            "readthedocs.io",
            "bioconductor.org",
            "github.com",
            "raw.githubusercontent.com",
        ],
        "domain_suffixes": [".github.io", ".readthedocs.io"],
    },
}


class HostNotAllowedError(RuntimeError):
    """Raised when a fetch targets a host outside the route allowlist."""


class EvidenceCapExceeded(RuntimeError):  # noqa: N818 - retained public exception name
    """Raised internally when the per-task evidence size cap is hit."""


def _record_retrieval_axis(
    out: Path,
    axis: str,
    query: str,
    *,
    status: str,
    **details: Any,
) -> None:
    """Persist every distinct axis/query attempt, including zero-result searches.

    A claims matrix contains no row for an axis that returned no evidence, so
    reconstructing retrieval scope from that matrix silently drops precisely
    the searches needed to distinguish "not found" from "not searched".
    Record the scope at the helper boundary, before network access. The lock
    keeps concurrent helper processes from losing one another's updates. Some
    survey tasks issue several candidate-method queries under one analysis
    axis, so axis alone is not a unique key: retain one record per axis/query
    pair and update only the matching attempt. The sorted payload remains
    deterministic.
    """
    out.mkdir(parents=True, exist_ok=True)
    scope_path = out / "retrieval_scope.json"
    lock_key = hashlib.sha256(str(out.resolve()).encode("utf-8")).hexdigest()
    # Keep synchronization bookkeeping outside the scientific output tree so
    # the package does not retain an unexplained lock file as an output.
    lock_path = Path("/tmp") / f"ecaa-literature-scope-{lock_key}.lock"
    with lock_path.open("a+", encoding="utf-8") as lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        payload: Dict[str, Any] = {
            "schema_version": RETRIEVAL_SCOPE_SCHEMA_VERSION,
            "axes": [],
        }
        if scope_path.is_file():
            try:
                loaded = json.loads(scope_path.read_text(encoding="utf-8"))
            except (OSError, ValueError) as exc:
                raise RuntimeError(
                    f"cannot update malformed retrieval scope {scope_path}: {exc}"
                ) from exc
            if not isinstance(loaded, dict) or not isinstance(loaded.get("axes"), list):
                raise RuntimeError(
                    f"cannot update malformed retrieval scope {scope_path}: "
                    "expected an object with an axes array"
                )
            payload = loaded

        by_attempt: Dict[tuple[str, str], Dict[str, Any]] = {}
        for item in payload.get("axes", []):
            if not isinstance(item, dict):
                continue
            recorded = str(item.get("axis") or "").strip()
            recorded_query = str(item.get("query") or "").strip()
            if recorded:
                by_attempt[(recorded, recorded_query)] = dict(item)

        attempt_key = (axis, query)
        entry = by_attempt.get(attempt_key, {"axis": axis, "query": query})
        entry["query"] = query
        entry["status"] = status
        entry.update(details)
        by_attempt[attempt_key] = entry

        stable = {
            "schema_version": RETRIEVAL_SCOPE_SCHEMA_VERSION,
            "axes": [by_attempt[key] for key in sorted(by_attempt)],
        }
        tmp_path = out / f".retrieval_scope.json.tmp-{os.getpid()}"
        tmp_path.write_text(
            json.dumps(stable, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        os.replace(tmp_path, scope_path)
        fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


# --------------------------------------------------------------------------
# Normalizer — must match the Rust `collapse_whitespace_lowercase_v1` exactly:
# collapse runs of whitespace to a single space, lowercase, trim.
# --------------------------------------------------------------------------


def normalize_text(s: str) -> str:
    return re.sub(r"\s+", " ", s).strip().lower()


_CANDIDATE_ALIAS_OVERRIDES: Dict[str, Tuple[str, ...]] = {
    "deseq2_vst": ("deseq2", "variance stabilizing transformation"),
    "edger_tmm": ("edger", "tmm", "trimmed mean of m-values"),
    "limma_voom": ("limma", "voom"),
    "seurat_lognormalize": ("seurat", "lognormalize"),
    "sctransform": ("sctransform",),
    "clusterprofiler": ("clusterprofiler",),
    "fgsea": ("fgsea",),
    "gsea": ("gsea", "gene set enrichment analysis"),
}

_GENERIC_CANDIDATE_TOKENS = {
    "analysis",
    "filter",
    "filtering",
    "method",
    "model",
    "modeling",
    "modelling",
    "normalization",
    "normalisation",
}


def candidate_aliases(candidate: str) -> Tuple[str, ...]:
    """Return conservative source-text aliases for a candidate method.

    Method-landscape retrieval must not treat a query hit as evidence for the
    requested candidate unless the retained source text actually names it.
    Curated compound ids use a small alias table; other ids retain their full
    normalized form plus distinctive component tokens.
    """
    key = normalize_text(candidate).replace("-", "_").replace(" ", "_")
    aliases = list(_CANDIDATE_ALIAS_OVERRIDES.get(key, ()))
    full = re.sub(r"[_-]+", " ", key).strip()
    if full:
        aliases.append(full)
    for token in re.findall(r"[a-z0-9]+", key):
        if len(token) >= 4 and token not in _GENERIC_CANDIDATE_TOKENS:
            aliases.append(token)
    return tuple(dict.fromkeys(a for a in aliases if a))


def _candidate_alias_match(text: str, candidate: str) -> Optional[re.Match[str]]:
    key = normalize_text(candidate).replace("-", "_").replace(" ", "_")
    # These canonical tool ids are ordinary English words when lower-cased.
    # Require their conventional all-caps spelling so a MAST-cell paper or a
    # generic "star" sentence cannot become method evidence.
    if key in {"mast", "star"}:
        return re.search(rf"(?<![A-Za-z0-9]){key.upper()}(?![A-Za-z0-9])", text)
    for alias in sorted(candidate_aliases(candidate), key=len, reverse=True):
        pattern = rf"(?<![a-z0-9]){re.escape(alias)}(?![a-z0-9])"
        match = re.search(pattern, text, flags=re.IGNORECASE)
        if match:
            return match
    return None


def candidate_evidence_quote(source_text: str, candidate: str) -> str:
    """Select a short verbatim source excerpt that names `candidate`.

    The previous first-sentence extractor often retained a generic background
    sentence while the method name appeared much later in the abstract. Such a
    row passed quote-presence validation but did not link the quote to the
    candidate. This selector fails closed when no candidate alias is present.
    """
    text = re.sub(r"\s+", " ", source_text or "").strip()
    if not text:
        return ""
    for sentence in re.split(r"(?<=[.!?])\s+", text):
        match = _candidate_alias_match(sentence, candidate)
        if not match:
            continue
        if len(sentence) <= 320:
            return sentence.strip()
        start = max(0, match.start() - 140)
        end = min(len(sentence), match.end() + 140)
        if start > 0:
            next_space = sentence.find(" ", start)
            start = next_space + 1 if next_space >= 0 else start
        if end < len(sentence):
            prior_space = sentence.rfind(" ", 0, end)
            end = prior_space if prior_space > start else end
        return sentence[start:end].strip()
    return ""


def _utc_now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _host_allowed(
    host: str,
    allowed_hosts: Iterable[str],
    domain_suffixes: Iterable[str] = (),
) -> bool:
    host = (host or "").lower()
    for h in allowed_hosts:
        h = h.lower()
        if host == h or host.endswith("." + h):
            return True
    for suf in domain_suffixes:
        if host.endswith(suf.lower()):
            return True
    return False


# --------------------------------------------------------------------------
# Network seam. Tests monkeypatch these two functions. Both enforce the
# host allowlist BEFORE issuing any request.
# --------------------------------------------------------------------------


def _http_get_json(url: str, host: str, allowed_hosts: List[str]) -> Any:
    if not _host_allowed(host, allowed_hosts):
        raise HostNotAllowedError(f"host {host!r} not in allowlist {allowed_hosts!r}")
    raw = _raw_get(url)
    return json.loads(raw.decode("utf-8", errors="replace"))


def _http_get_text(url: str, host: str, allowed_hosts: List[str]) -> str:
    # `allowed_hosts` may carry leading-dot suffix entries (e.g.
    # `.readthedocs.io`); `_host_allowed` treats those as suffix matches.
    suffixes = [h for h in allowed_hosts if h.startswith(".")]
    hosts = [h for h in allowed_hosts if not h.startswith(".")]
    if not _host_allowed(host, hosts, suffixes):
        raise HostNotAllowedError(f"host {host!r} not in allowlist {allowed_hosts!r}")
    raw = _raw_get(url)
    return raw.decode("utf-8", errors="replace")


def _raw_get(url: str) -> bytes:
    attempt = 0
    while True:
        # Pace: keep at least _MIN_REQUEST_INTERVAL between egress calls so a
        # burst of esearch+efetch stays under NCBI's per-IP rate limit.
        wait = _MIN_REQUEST_INTERVAL - (_time.monotonic() - _last_request_monotonic[0])
        if wait > 0:
            _time.sleep(wait)
        _last_request_monotonic[0] = _time.monotonic()
        req = Request(url, headers={"User-Agent": USER_AGENT, "Accept": "*/*"})
        try:
            with urlopen(req, timeout=HTTP_TIMEOUT_SECS) as resp:  # noqa: S310 (bounded host)
                return resp.read()
        except HTTPError as e:
            # 429 (rate limit) / 503 (transient) are retryable; back off and
            # retry up to _HTTP_MAX_RETRIES, honoring a numeric Retry-After.
            if e.code in (429, 503) and attempt < _HTTP_MAX_RETRIES:
                retry_after = e.headers.get("Retry-After") if e.headers else None
                try:
                    backoff = float(retry_after) if retry_after else 2.0 ** attempt
                except (TypeError, ValueError):
                    backoff = 2.0 ** attempt
                _time.sleep(min(backoff, _HTTP_MAX_BACKOFF_SECS))
                attempt += 1
                continue
            raise


# --------------------------------------------------------------------------
# Source-class enablement.
# --------------------------------------------------------------------------


def enabled_classes(classes: Optional[List[str]]) -> List[str]:
    """Resolve the enabled source classes.

    When `classes` is explicitly provided (caller decided), use it. When
    None, derive from the environment, defaulting to primary_literature
    only when the scope env is unset. A later workstream owns the richer
    authority-mode knob; this is a minimal default, not the policy.
    """
    if classes is not None:
        return list(classes)
    # All current scope tiers (pmc_oa, pmc_oa_plus_abstracts,
    # all_sources_local_only) gate the primary-literature path; proceedings
    # and tool-docs are opt-in by the caller / a later authority knob.
    return ["primary_literature"]


def _evidence_cap_bytes() -> Optional[int]:
    raw = os.environ.get("ECAA_LIT_EVIDENCE_MAX_MB", "").strip()
    if not raw:
        return None
    try:
        mb = int(raw)
    except ValueError:
        return None
    if mb <= 0:
        return None
    return mb * 1024 * 1024


# --------------------------------------------------------------------------
# OpenAlex / Crossref helpers (conference_proceedings).
# --------------------------------------------------------------------------


def _openalex_reconstruct_abstract(inv_index: Dict[str, List[int]]) -> str:
    """Rebuild plain-text abstract from OpenAlex's inverted index."""
    if not inv_index:
        return ""
    positions: List[Tuple[int, str]] = []
    for word, idxs in inv_index.items():
        for i in idxs:
            positions.append((i, word))
    positions.sort(key=lambda p: p[0])
    return " ".join(w for _, w in positions)


def _strip_doi(doi: str) -> str:
    """Normalize an OpenAlex/Crossref DOI to its bare `10.x/...` form."""
    doi = (doi or "").strip()
    doi = re.sub(r"^https?://(dx\.)?doi\.org/", "", doi, flags=re.IGNORECASE)
    return doi


def _openalex_extract(results: List[Dict[str, Any]]) -> List[Dict[str, str]]:
    """Extract (candidate, doi, quote) tuples from OpenAlex results."""
    out = []
    for r in results or []:
        doi = _strip_doi(r.get("doi", ""))
        if not doi:
            continue
        title = (r.get("display_name") or r.get("title") or "").strip()
        abstract = _openalex_reconstruct_abstract(r.get("abstract_inverted_index") or {})
        quote = abstract or title
        if not (title and quote):
            continue
        out.append({"candidate": title, "source_ref": doi, "quote": quote})
    return out


def _crossref_extract(payload: Dict[str, Any]) -> List[Dict[str, str]]:
    """Extract (candidate, doi, quote) tuples from a Crossref response."""
    out = []
    items = ((payload or {}).get("message") or {}).get("items") or []
    for it in items:
        doi = _strip_doi(it.get("DOI", ""))
        titles = it.get("title") or []
        title = (titles[0] if titles else "").strip()
        abstract = re.sub(r"<[^>]+>", " ", it.get("abstract", "") or "")
        quote = (abstract.strip() or title)
        if not (doi and title and quote):
            continue
        out.append({"candidate": title, "source_ref": doi, "quote": quote})
    return out


# --------------------------------------------------------------------------
# Tool-documentation helpers.
# --------------------------------------------------------------------------

_VERSION_NEAR_TOOL = re.compile(r"\bv?(\d+(?:\.\d+){0,2})\b", re.IGNORECASE)


def _strip_html(html: str) -> str:
    text = re.sub(r"(?is)<(script|style)[^>]*>.*?</\1>", " ", html)
    text = re.sub(r"<[^>]+>", " ", text)
    return text


def _extract_version_context(text: str, candidate: str) -> Optional[str]:
    """Find a version token near the candidate tool name in the doc text."""
    norm = text
    low = norm.lower()
    cand_low = candidate.lower()
    pos = low.find(cand_low)
    if pos < 0:
        return None
    window = norm[pos : pos + len(candidate) + 40]
    m = _VERSION_NEAR_TOOL.search(window[len(candidate):])
    if m:
        return m.group(1)
    return None


# --------------------------------------------------------------------------
# Snapshot + manifest + CSV emit.
# --------------------------------------------------------------------------


def _snapshot(ev_dir: Path, payload: bytes) -> Tuple[str, str]:
    """Write `payload` to evidence/snapshots/<sha256>; return (relpath, sha)."""
    sha = hashlib.sha256(payload).hexdigest()
    snap_dir = ev_dir / "snapshots"
    snap_dir.mkdir(parents=True, exist_ok=True)
    rel = f"snapshots/{sha}"
    target = snap_dir / sha
    if not target.exists():
        target.write_bytes(payload)
    return rel, sha


def _load_manifest(manifest_path: Path) -> Dict[str, Any]:
    if manifest_path.exists():
        try:
            m = json.loads(manifest_path.read_text())
            if isinstance(m, dict) and isinstance(m.get("entries"), list):
                m.setdefault("schema_version", MANIFEST_SCHEMA_VERSION)
                return m
        except (ValueError, OSError):
            pass
    return {"schema_version": MANIFEST_SCHEMA_VERSION, "entries": []}


def _write_manifest(manifest_path: Path, manifest: Dict[str, Any]) -> None:
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=False) + "\n")


def _append_csv_rows(csv_path: Path, rows: List[Dict[str, Any]]) -> None:
    new_file = not csv_path.exists()
    with csv_path.open("a", newline="") as fh:
        writer = csv.DictWriter(fh, fieldnames=CSV_COLUMNS, extrasaction="ignore")
        if new_file:
            writer.writeheader()
        for r in rows:
            writer.writerow(r)


def _next_query_id(manifest: Dict[str, Any]) -> str:
    """Return the next `q###` id not already used in the manifest."""
    used = set()
    for e in manifest.get("entries", []):
        qid = e.get("retrieval_query_id", "")
        m = re.match(r"^q(\d+)$", qid)
        if m:
            used.add(int(m.group(1)))
    n = 1
    while n in used:
        n += 1
    return f"q{n:03d}"


# --------------------------------------------------------------------------
# Per-class fetch routines, each returning a list of "finding" dicts.
# --------------------------------------------------------------------------


def _fetch_conference_proceedings(query: str, route: Dict[str, Any]) -> List[Dict[str, str]]:
    hosts = route.get("hosts") or DEFAULT_ROUTES["conference_proceedings"]["hosts"]
    findings: List[Dict[str, str]] = []
    # OpenAlex is the primary proceedings index; Crossref is the fallback.
    if any("openalex" in h for h in hosts):
        host = next(h for h in hosts if "openalex" in h)
        from urllib.parse import quote as _q

        url = f"https://{host}/works?search={_q(query)}&per-page=5"
        payload = _http_get_json(url, host, hosts)
        results = payload.get("results") if isinstance(payload, dict) else None
        for f in _openalex_extract(results or []):
            f["source_kind"] = "openalex"
            findings.append(f)
    elif any("crossref" in h for h in hosts):
        host = next(h for h in hosts if "crossref" in h)
        from urllib.parse import quote as _q

        url = f"https://{host}/works?query={_q(query)}&rows=5"
        payload = _http_get_json(url, host, hosts)
        for f in _crossref_extract(payload if isinstance(payload, dict) else {}):
            f["source_kind"] = "crossref"
            findings.append(f)
    return findings


def _pubmed_extract_abstract(xml: str) -> Tuple[str, str, str]:
    """Parse one PubMed efetch XML record → (pmid, title, abstract_text).

    Concatenates every <AbstractText> (structured abstracts split it into
    labelled sections). Returns empty strings for any field absent so the
    caller can skip a record with no usable abstract."""
    import xml.etree.ElementTree as ET

    try:
        root = ET.fromstring(xml)
    except ET.ParseError:
        return "", "", ""
    art = root.find(".//PubmedArticle")
    if art is None:
        art = root
    pmid_el = art.find(".//MedlineCitation/PMID")
    pmid = (pmid_el.text or "").strip() if pmid_el is not None else ""
    title_el = art.find(".//Article/ArticleTitle")
    title = "".join(title_el.itertext()).strip() if title_el is not None else ""
    abstract = " ".join(
        "".join(a.itertext()).strip()
        for a in art.findall(".//Abstract/AbstractText")
    ).strip()
    return pmid, title, abstract


def _pubmed_evidence_quote(abstract: str) -> str:
    """Pick a verbatim quote from an abstract: the first sentence (capped),
    falling back to the whole abstract. Returned VERBATIM so the downstream
    quote-presence check against the stored snapshot is exact.

    NOTE: the quote is a SHORT verbatim excerpt used only for the
    quote-presence check; the snapshot stored to disk is the FULL abstract
    (see the payload-snapshot site), not this excerpt. The quote-presence
    check confirms the excerpt was copied verbatim from the source — it does
    NOT assess whether the source supports a claim or its directionality."""
    abstract = abstract.strip()
    if not abstract:
        return ""
    for end in (". ", "? ", "! "):
        idx = abstract.find(end)
        if 0 < idx <= 280:
            return abstract[: idx + 1].strip()
    return abstract[:280].strip()


def _fetch_primary_literature(query: str, route: Dict[str, Any]) -> List[Dict[str, Any]]:
    """PubMed esearch→efetch retrieval emitting one finding per PMID.

    esearch (JSON) maps the query to PMIDs; efetch (XML) fetches each record's
    abstract. Each finding carries the SINGULAR pmid as its locator so the
    snapshot/manifest plumbing writes a per-PMID entry — which is exactly what
    the harness literature validators resolve against (run_pmid_resolves keys
    the manifest by singular pmid; the helper explicitly records the package
    policy's `abstract_fair_use` basis for `pubmed_abstract`). PubMed delivery
    does not make publisher-supplied abstract text public domain. The snapshot
    bytes ARE the FULL extracted abstract text (carried as `_extracted`), so the
    evidence_quote quote-presence check is exact and the stored snapshot is a
    faithful record of the whole abstract — not just its topic sentence.

    Best-effort and bounded: at most `retmax` PMIDs; a record with no abstract
    or a malformed efetch response is skipped (a transport failure propagates
    to fetch_for_axis's per-class try/except → curated fallback)."""
    hosts = route.get("hosts") or DEFAULT_ROUTES["primary_literature"]["hosts"]
    host = next((h for h in hosts if "eutils" in h), hosts[0])
    from urllib.parse import quote as _q

    retmax = int(route.get("retmax", 6))
    api_key = os.environ.get("ECAA_LIT_NCBI_API_KEY", "").strip()
    key_qs = f"&api_key={api_key}" if api_key else ""

    esearch_url = (
        f"https://{host}/entrez/eutils/esearch.fcgi?db=pubmed"
        f"&retmode=json&retmax={retmax}&term={_q(query)}{key_qs}"
    )
    payload = _http_get_json(esearch_url, host, hosts)
    idlist = []
    if isinstance(payload, dict):
        idlist = (payload.get("esearchresult") or {}).get("idlist") or []

    findings: List[Dict[str, Any]] = []
    for pmid in idlist:
        pmid = str(pmid).strip()
        if not pmid.isdigit():
            continue
        efetch_url = (
            f"https://{host}/entrez/eutils/efetch.fcgi?db=pubmed"
            f"&retmode=xml&id={pmid}{key_qs}"
        )
        xml = _http_get_text(efetch_url, host, hosts)
        got_pmid, title, abstract = _pubmed_extract_abstract(xml)
        quote = _pubmed_evidence_quote(abstract)
        if not quote:
            continue
        findings.append(
            {
                "candidate": title or query,
                "source_ref": got_pmid or pmid,
                "pmid": got_pmid or pmid,
                "source_kind": "pubmed_abstract",
                # snapshot bytes = the FULL extracted abstract (`_extracted`);
                # `quote` is a short verbatim excerpt within it, so the
                # quote-presence check is exact.
                "quote": quote,
                "_extracted": abstract,
            }
        )
    return findings


def _fetch_tool_documentation(query: str, route: Dict[str, Any]) -> List[Dict[str, Any]]:
    hosts = route.get("hosts") or DEFAULT_ROUTES["tool_documentation"]["hosts"]
    suffixes = route.get("domain_suffixes") or DEFAULT_ROUTES["tool_documentation"].get(
        "domain_suffixes", []
    )
    doc_urls = route.get("doc_urls") or []
    candidate = route.get("candidate") or query
    findings: List[Dict[str, Any]] = []
    # Fold the leading-dot domain suffixes into the allowlist the seam
    # receives, so `_http_get_text` keeps the uniform (url, host,
    # allowed_hosts) signature that tests stub.
    allow = list(hosts) + [s if s.startswith(".") else "." + s for s in suffixes]
    for url in doc_urls:
        host = urlparse(url).hostname or ""
        html = _http_get_text(url, host, allow)
        text = _strip_html(html)
        version = _extract_version_context(text, candidate)
        # Build a concise verbatim quote: the sentence-ish window around the
        # candidate mention, falling back to the whole normalized text.
        low = text.lower()
        idx = low.find(candidate.lower())
        if idx >= 0:
            quote = text[idx : idx + 120].strip()
        else:
            quote = text.strip()[:120]
        findings.append(
            {
                "candidate": candidate,
                "source_ref": url,
                "quote": quote,
                "source_kind": "doc_page",
                "version_context": version,
                "_raw": html,
                # Extracted plain text used for substring verification;
                # the raw HTML is what gets snapshotted to disk.
                "_extracted": text,
            }
        )
    return findings


# --------------------------------------------------------------------------
# Public API.
# --------------------------------------------------------------------------


def fetch_for_axis(
    out_dir: str,
    axis: str,
    query: str,
    classes: Optional[List[str]] = None,
    routes: Optional[Dict[str, Dict[str, Any]]] = None,
    curated: Optional[List[str]] = None,
    candidate: Optional[str] = None,
) -> Dict[str, Any]:
    """Retrieve method-landscape evidence for one analysis `axis`.

    Queries the enabled source classes' indexes, extracts
    (candidate, locator, quote[, version]) tuples, snapshots every fetched
    source under `<out_dir>/evidence/snapshots/<sha256>`, writes/extends
    `<out_dir>/evidence/manifest.json`, and appends rows to
    `<out_dir>/method_landscape.csv`.

    `curated` is the axis's curated candidate pool (the task spec's
    `attributes.candidate_tools`). When retrieval yields zero usable rows for
    the axis — offline, every route fails, or the literature is thin — the
    helper falls back to emitting one `curated_baseline` row per curated
    candidate (no locator, `verified=false`, empty quote). The fallback never
    raises and never blocks the task.

    Always (re)writes a `<out_dir>/method_landscape.json` rollup from the
    full CSV so a sibling UI agent has the per-axis candidate view.

    Returns a small summary dict (counts) for the caller's log.
    """
    out = Path(out_dir)
    ev_dir = out / "evidence"
    ev_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = ev_dir / "manifest.json"
    csv_path = out / "method_landscape.csv"
    _record_retrieval_axis(out, axis, query, status="attempted")

    routes = routes or {}
    curated = list(curated or [])
    active = enabled_classes(classes)
    cap = _evidence_cap_bytes()

    manifest = _load_manifest(manifest_path)
    ev_used = sum(int(e.get("bytes", 0)) for e in manifest.get("entries", []))
    truncated = False
    rows_out: List[Dict[str, Any]] = []
    n_entries = 0
    candidate_mismatch_filtered = 0

    for cls in active:
        route = routes.get(cls, {}) or DEFAULT_ROUTES.get(cls, {})
        # Retrieval is best-effort: a transport/availability failure (offline,
        # DNS failure, timeout, malformed response) must degrade to the
        # curated-baseline fallback below, never propagate and block the task.
        # An egress-allowlist VIOLATION is a separate, loud failure (below).
        try:
            if cls == "conference_proceedings":
                findings = _fetch_conference_proceedings(query, route)
                ref_kind = "doi"
                evidence_role = "recommendation_or_benchmark"
            elif cls == "tool_documentation":
                findings = _fetch_tool_documentation(query, route)
                ref_kind = "url"
                evidence_role = "capability_or_version"
            elif cls == "primary_literature":
                findings = _fetch_primary_literature(query, route)
                ref_kind = "pmid"
                evidence_role = "recommendation_or_benchmark"
            else:
                findings = []
                ref_kind = "url"
                evidence_role = "recommendation_or_benchmark"
        except HostNotAllowedError:
            # An egress-allowlist violation is a route MISCONFIGURATION, not a
            # transport/availability failure. Surface it loudly rather than
            # silently degrading — the curated fallback is for offline / route
            # failure / thin literature, not for a bad allowlist.
            raise
        except Exception as exc:  # noqa: BLE001 — transport failure → fallback
            # Offline, DNS failure, timeout, connection reset, malformed
            # response, etc.: degrade to the curated-baseline fallback below
            # rather than block the task.
            sys.stderr.write(
                f"[literature-fetch] axis={axis!r} class={cls!r} retrieval "
                f"failed ({type(exc).__name__}: {exc}); falling back to "
                f"curated pool.\n"
            )
            continue

        for f in findings:
            # Candidate override: when the caller surveys a NAMED method
            # (survey_method_landscape calls the helper once per candidate
            # method), every retrieved source is tagged with that method so
            # multiple PMIDs accumulate under ONE candidate — which is what the
            # corroboration validator (≥`min_sources` distinct verified PMIDs
            # per candidate) requires. Without it each paper becomes its own
            # single-PMID candidate and no axis ever carries a valid default.
            if candidate and cls != "tool_documentation":
                relevant_quote = candidate_evidence_quote(
                    f.get("_extracted", f.get("quote", "")), candidate
                )
                if not relevant_quote:
                    candidate_mismatch_filtered += 1
                    continue
                f["quote"] = relevant_quote
            if candidate:
                f["candidate"] = candidate
            # Snapshot bytes: tool-doc keeps the raw HTML; index hits store the
            # FULL extracted source text (`_extracted`, e.g. the whole PubMed
            # abstract) when present, so the snapshot is a faithful record of
            # the source rather than a ~100-byte topic sentence. The
            # quote-presence check below substring-matches the verbatim quote
            # against this full text, so it stays exact and reproducible. Only
            # when a finding carries no `_extracted` (e.g. a curated baseline
            # row whose quote IS its whole text) do we fall back to the quote.
            if cls == "tool_documentation":
                payload = f["_raw"].encode("utf-8")
            else:
                payload = f.get("_extracted", f["quote"]).encode("utf-8")

            if cap is not None and ev_used + len(payload) > cap:
                truncated = True
                break

            rel, sha = _snapshot(ev_dir, payload)
            ev_used += len(payload)
            n_entries += 1
            ts = _utc_now_iso()
            qid = _next_query_id(manifest)

            # quote_present := the verbatim quote substring-matches the
            # source's EXTRACTED text after collapse_whitespace_lowercase_v1
            # normalization. This is a QUOTE-PRESENCE check only: it confirms
            # the quote was copied verbatim from the snapshotted source. It does
            # NOT assess whether the source SUPPORTS a downstream claim, nor the
            # DIRECTIONALITY of any effect. For literature/index hits the
            # snapshot binary now holds the FULL extracted text (`_extracted`),
            # so the quote is matched against the whole abstract; for tool-doc
            # pages the binary is raw HTML, so we verify against the stripped
            # text (`_extracted`).
            extracted_src = f.get("_extracted", payload.decode("utf-8", errors="replace"))
            snap_norm = normalize_text(extracted_src)
            quote_norm = normalize_text(f["quote"])
            quote_present = bool(quote_norm) and quote_norm in snap_norm
            verified = quote_present
            offset = snap_norm.find(quote_norm) if verified else 0

            extracted_sha = hashlib.sha256(snap_norm.encode("utf-8")).hexdigest()
            # Proceedings snapshots store only short verbatim quotes (fair
            # use); tool-doc HTML carries an unknown license, so mark it
            # non-redistributable conservatively.
            redistributable = cls != "tool_documentation"
            license_str = "unknown" if cls == "tool_documentation" else "abstract_fair_use"

            entry: Dict[str, Any] = {
                "source_kind": f["source_kind"],
                "source_ref_kind": ref_kind,
                "source_ref": f["source_ref"],
                "source_class": cls,
                "evidence_role": evidence_role,
                "path": rel,
                "sha256_binary": sha,
                "sha256_extracted_text": extracted_sha,
                "extracted_text_normalization": EXTRACTED_TEXT_NORMALIZATION,
                "bytes": len(payload),
                "retrieval_ts": ts,
                "retrieval_query_id": qid,
                "redistributable": redistributable,
                "license": license_str,
            }
            if f.get("version_context"):
                entry["version_context"] = f["version_context"]
            # Singular `pmid` on the manifest entry AND the CSV row for
            # PMID-locator findings: the harness `run_pmid_resolves` validator
            # keys the manifest by singular `pmid`, and the claims-matrix
            # `pmid` column anchors the per-row resolve. Without it a
            # legitimately-retrieved abstract reports pmid_not_found.
            pmid_val = str(f.get("pmid") or "") if ref_kind == "pmid" else ""
            if pmid_val:
                entry["pmid"] = pmid_val
            manifest["entries"].append(entry)

            row = {
                "axis": axis,
                "candidate_method": f["candidate"],
                "source_ref_kind": ref_kind,
                "source_ref": f["source_ref"],
                "source_class": cls,
                "evidence_role": evidence_role,
                "evidence_quote": f["quote"],
                "evidence_quote_offset": offset,
                "source_kind": f["source_kind"],
                "source_hash": "sha256:" + sha,
                "retrieval_ts": ts,
                "redistributable": "true" if redistributable else "false",
                "verified": "true" if verified else "false",
                "version_context": f.get("version_context") or "",
                "pmid": pmid_val,
            }
            rows_out.append(row)

        if truncated:
            break

    _write_manifest(manifest_path, manifest)

    # Curated fallback: when retrieval produced zero usable rows for this axis
    # (offline, all routes failed, or thin literature), seed the axis from the
    # curated candidate pool so the downstream discover_* atom still has
    # something to rank. Curated-baseline rows carry no locator and are never
    # verified; the validators skip them (no source_resolves obligation, kept
    # out of the corroboration tier) so nothing blocks.
    fallback_used = False
    if rows_out:
        _append_csv_rows(csv_path, rows_out)
    else:
        # A per-candidate survey call must only emit a fallback for that
        # candidate. Emitting the full axis pool on every zero-result query
        # duplicates unrelated curated rows and can inflate downstream support
        # counts. The full pool is still retained in curated_pools.json below.
        fallback_candidates = [candidate] if candidate else curated
        fallback_rows = _curated_baseline_rows(axis, fallback_candidates)
        if fallback_rows:
            fallback_used = True
            _append_csv_rows(csv_path, fallback_rows)
        elif not csv_path.exists():
            # No curated pool either — still leave a header-only CSV so the
            # downstream loader + the required_artifacts check find the file.
            _append_csv_rows(csv_path, [])

    # The method_landscape.json rollup is (re)written from the full CSV in
    # BOTH the normal and the fallback path so a sibling UI agent always has a
    # current per-axis candidate view. The CSV accumulates rows across the
    # per-axis calls the agent makes, so the curated pools must accumulate too
    # (otherwise a later axis's rebuild would forget an earlier axis's pool and
    # mark its curated candidates tentative). Persist per-axis pools in a small
    # sidecar and merge the current axis in before rebuilding.
    curated_by_axis = _merge_curated_pool(ev_dir / "curated_pools.json", axis, curated)
    _write_method_landscape_json(
        out / "method_landscape.json",
        csv_path,
        curated_by_axis=curated_by_axis,
    )

    summary = {
        "axis": axis,
        "entries_written": n_entries,
        "rows_written": len(rows_out),
        "fallback_used": fallback_used,
        "candidate_mismatch_filtered": candidate_mismatch_filtered,
        "truncated_at_storage_cap": truncated,
    }
    _record_retrieval_axis(
        out,
        axis,
        query,
        status="completed",
        entries_written=n_entries,
        rows_written=len(rows_out),
        fallback_used=fallback_used,
        candidate_mismatch_filtered=candidate_mismatch_filtered,
        truncated_at_storage_cap=truncated,
    )
    if fallback_used:
        # Soft-warning: this axis fell back to curated_baseline rows only (no
        # live source resolved/verified). The Phase-13 validators SKIP
        # curated_baseline rows, so the survey passes GREEN while carrying zero
        # verified literature evidence for this axis. Surface it so a green
        # offline run does not masquerade as real retrieval (warn-only, by
        # design never-block).
        summary["warning"] = (
            f"axis '{axis}' fell back to curated_baseline only: no live source "
            "was resolved/verified, so this axis contributes zero verified "
            "evidence (validators skip curated_baseline rows)."
        )
    if truncated:
        summary["truncated_at_storage_cap"] = True
    return summary


def _curated_baseline_rows(axis: str, curated: List[str]) -> List[Dict[str, Any]]:
    """One `curated_baseline` row per curated candidate for `axis`.

    These rows carry no locator (`source_ref_kind`/`source_ref` empty), an
    empty `evidence_quote`, and `verified=false`. They exist only so a
    discover_* atom can still offer the curated pool when literature retrieval
    was unavailable; they are explicitly excluded from the locator-resolution
    and corroboration validators.
    """
    ts = _utc_now_iso()
    rows: List[Dict[str, Any]] = []
    for cand in curated:
        cand = (cand or "").strip()
        if not cand:
            continue
        rows.append(
            {
                "axis": axis,
                "candidate_method": cand,
                "source_ref_kind": "",
                "source_ref": "",
                "source_class": "curated_baseline",
                "evidence_role": "",
                "evidence_quote": "",
                "evidence_quote_offset": 0,
                "source_kind": "",
                "source_hash": "",
                "retrieval_ts": ts,
                "redistributable": "true",
                "verified": "false",
                "version_context": "",
            }
        )
    return rows


# --------------------------------------------------------------------------
# method_landscape.json rollup. Conforms to the shape the sibling UI agent
# consumes; derived from the full method_landscape.csv (all axes).
# --------------------------------------------------------------------------

# Paper-class source classes that make a candidate literature_eligible and
# count toward its support_score (mirrors the Rust `PAPER_CLASSES`).
PAPER_CLASSES = ("primary_literature", "conference_proceedings")

METHOD_LANDSCAPE_JSON_SCHEMA_VERSION = 1


def _read_csv_dicts(csv_path: Path) -> List[Dict[str, str]]:
    if not csv_path.exists():
        return []
    with csv_path.open(newline="") as fh:
        return list(csv.DictReader(fh))


def _ref_or_none(value: str) -> Optional[str]:
    value = (value or "").strip()
    return value or None


def build_method_landscape_rollup(
    csv_rows: List[Dict[str, str]],
    curated_by_axis: Optional[Dict[str, List[str]]] = None,
) -> Dict[str, Any]:
    """Build the `method_landscape.json` document from method_landscape.csv rows.

    Shape (consumed verbatim by the UI agent):
        {"schema_version": 1,
         "axes": {"<axis>": {"candidates": [
            {"method", "literature_eligible", "tentative", "support_score",
             "evidence": [{"source_class", "source_ref_kind", "source_ref",
                           "evidence_quote", "version_context"}]}]}}}

    - `literature_eligible`: candidate has ≥1 verified row whose source_class
      is paper-class (primary_literature | conference_proceedings).
    - `support_score`: count of verified paper-class evidence rows
      (curated_baseline rows contribute 0).
    - `tentative`: candidate NOT in the axis's curated pool.
    - candidates sorted by support_score desc, then method name asc.
    """
    curated_by_axis = curated_by_axis or {}
    # axis -> candidate -> accumulator
    axes: Dict[str, Dict[str, Dict[str, Any]]] = {}
    for r in csv_rows:
        axis = (r.get("axis") or "").strip()
        cand = (r.get("candidate_method") or "").strip()
        if not axis or not cand:
            continue
        cls = (r.get("source_class") or "").strip()
        verified = (r.get("verified") or "").strip().lower() == "true"
        acc = axes.setdefault(axis, {}).setdefault(
            cand, {"literature_eligible": False, "support_score": 0, "evidence": []}
        )
        if verified and cls in PAPER_CLASSES:
            acc["literature_eligible"] = True
            acc["support_score"] += 1
        acc["evidence"].append(
            {
                "source_class": cls,
                "source_ref_kind": _ref_or_none(r.get("source_ref_kind", "")),
                "source_ref": _ref_or_none(r.get("source_ref", "")),
                "evidence_quote": r.get("evidence_quote", "") or "",
                "version_context": _ref_or_none(r.get("version_context", "")),
            }
        )

    out_axes: Dict[str, Any] = {}
    for axis, cands in axes.items():
        curated_pool = set(curated_by_axis.get(axis, []) or [])
        candidate_list: List[Dict[str, Any]] = []
        for cand, acc in cands.items():
            candidate_list.append(
                {
                    "method": cand,
                    "literature_eligible": bool(acc["literature_eligible"]),
                    "tentative": cand not in curated_pool,
                    "support_score": int(acc["support_score"]),
                    "evidence": acc["evidence"],
                }
            )
        candidate_list.sort(key=lambda c: (-c["support_score"], c["method"]))
        out_axes[axis] = {"candidates": candidate_list}

    return {
        "schema_version": METHOD_LANDSCAPE_JSON_SCHEMA_VERSION,
        "axes": out_axes,
    }


def _write_method_landscape_json(
    json_path: Path,
    csv_path: Path,
    curated_by_axis: Optional[Dict[str, List[str]]] = None,
) -> None:
    rollup = build_method_landscape_rollup(_read_csv_dicts(csv_path), curated_by_axis)
    json_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps(rollup, indent=2, sort_keys=False) + "\n")


def _merge_curated_pool(
    sidecar_path: Path, axis: str, curated: List[str]
) -> Dict[str, List[str]]:
    """Accumulate the per-axis curated pools across `fetch_for_axis` calls.

    The agent calls the helper once per axis; each call appends rows to a
    single `method_landscape.csv`. To rebuild the rollup correctly we need
    every axis's curated pool, so persist them in `evidence/curated_pools.json`
    and merge the current axis's pool in. Returns the full axis→pool map.
    """
    pools: Dict[str, List[str]] = {}
    if sidecar_path.exists():
        try:
            loaded = json.loads(sidecar_path.read_text())
            if isinstance(loaded, dict):
                pools = {k: list(v) for k, v in loaded.items() if isinstance(v, list)}
        except (ValueError, OSError):
            pools = {}
    if curated:
        # Union with any previously-recorded pool for this axis (dedupe,
        # preserve first-seen order).
        existing = pools.get(axis, [])
        merged: List[str] = list(existing)
        for c in curated:
            if c not in merged:
                merged.append(c)
        pools[axis] = merged
    else:
        pools.setdefault(axis, [])
    sidecar_path.parent.mkdir(parents=True, exist_ok=True)
    sidecar_path.write_text(json.dumps(pools, indent=2, sort_keys=True) + "\n")
    return pools


# --------------------------------------------------------------------------
# CLI entry point: `python agent_literature_fetch.py <out_dir> <axis> <query>`
# Optional trailing args are source classes; routes default per class. Pass
# the axis's curated candidate pool via `--curated a,b,c` so the helper can
# fall back to it when retrieval yields nothing. Intended to be called by the
# agent once per axis. Network egress is host-bounded.
# --------------------------------------------------------------------------


def _main(argv: List[str]) -> int:
    args = list(argv[1:])
    curated: Optional[List[str]] = None
    candidate: Optional[str] = None
    # Extract the optional `--curated a,b,c` / `--candidate <method>` flags
    # wherever they appear so the positional `[class ...]` tail stays
    # backward-compatible.
    rest: List[str] = []
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--curated" and i + 1 < len(args):
            curated = [c.strip() for c in args[i + 1].split(",") if c.strip()]
            i += 2
            continue
        if a.startswith("--curated="):
            curated = [c.strip() for c in a.split("=", 1)[1].split(",") if c.strip()]
            i += 1
            continue
        if a == "--candidate" and i + 1 < len(args):
            candidate = args[i + 1].strip() or None
            i += 2
            continue
        if a.startswith("--candidate="):
            candidate = a.split("=", 1)[1].strip() or None
            i += 1
            continue
        rest.append(a)
        i += 1
    if len(rest) < 3:
        sys.stderr.write(
            "usage: agent_literature_fetch.py <out_dir> <axis> <query> "
            "[class ...] [--curated a,b,c] [--candidate <method>]\n"
        )
        return 2
    out_dir, axis, query = rest[0], rest[1], rest[2]
    classes = rest[3:] or None
    summary = fetch_for_axis(
        out_dir=out_dir,
        axis=axis,
        query=query,
        classes=classes,
        curated=curated,
        candidate=candidate,
    )
    sys.stdout.write(json.dumps(summary) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(_main(sys.argv))
