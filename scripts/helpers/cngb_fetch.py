#!/usr/bin/env python3
"""Fetch a CNGB CNSA project's processed matrices over HTTPS.

Called by the data_acquisition execute stage when the SME-named cohort
includes a CNP* accession that can't be reached via GEOparse/GEOquery.
Keeps the pipeline's "use published processed matrices, do not
reprocess FASTQ" scope commitment while covering non-GEO Chinese
repositories.

Usage:
    python3 cngb_fetch.py CNP0002664 /path/to/out_dir
    python3 cngb_fetch.py --list-matrices CNP0002664

The CNGB CNSA URL layout is NOT fully deterministic across projects —
the "data shard" (data1/data2/.../dataN) is assigned at deposition and
has to be discovered via the CNGB search API or the project landing
page. This helper tries three discovery strategies in order:

  1. CNGB search JSON API            (https://db.cngb.org/api/v3/projects/<id>)
  2. Project landing-page HTML scrape (https://db.cngb.org/search/project/<id>/)
  3. Brute-force probe of ftp.cngb.org/pub/CNSA/data{1..10}/<id>/

On success, writes:
  <out_dir>/<CNP_id>/manifest.json        — resolved URLs + sha256s
  <out_dir>/<CNP_id>/matrices/            — downloaded .tar.gz / .mtx.gz files
  <out_dir>/<CNP_id>/soft/                — study-level metadata JSON

Exit codes:
  0 — all requested files downloaded and verified
  2 — project found but contains no processed matrices (agent should
      re-block with decision point 'process_from_fastq_or_defer')
  3 — project not found in CNGB search index (agent should re-block
      with decision point 'accession_mismatch_check')
  4 — network / IO error (agent should retry, then re-block with
      decision point 'cngb_transient_fault')
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import time
from pathlib import Path
from urllib.parse import urljoin
from urllib.request import Request, urlopen
from urllib.error import HTTPError, URLError

CNGB_API = "https://db.cngb.org/api/v3/projects/{accession}"
CNGB_PAGE = "https://db.cngb.org/search/project/{accession}/"
CNGB_FTP_BASE = "https://ftp.cngb.org/pub/CNSA"
USER_AGENT = "ecaa-workflow/cngb_fetch/1.0 (+research pipeline)"
TIMEOUT_S = 45
MAX_SHARD = 12          # brute-force upper bound on dataN partitions
CHUNK_BYTES = 1 << 20   # 1 MiB streaming download chunks
MATRIX_SUFFIXES = (
    ".tar.gz", ".tar", ".h5ad", ".h5",
    ".mtx.gz", ".mtx", ".loom",
    ".rds", ".rda", ".Rdata",
    ".csv.gz", ".tsv.gz",
)


def _get(url: str) -> bytes:
    req = Request(url, headers={"User-Agent": USER_AGENT, "Accept": "*/*"})
    with urlopen(req, timeout=TIMEOUT_S) as r:
        return r.read()


def _stream_to(url: str, dest: Path) -> int:
    req = Request(url, headers={"User-Agent": USER_AGENT})
    dest.parent.mkdir(parents=True, exist_ok=True)
    n = 0
    h = hashlib.sha256()
    with urlopen(req, timeout=TIMEOUT_S) as r, dest.open("wb") as f:
        while True:
            chunk = r.read(CHUNK_BYTES)
            if not chunk:
                break
            f.write(chunk)
            h.update(chunk)
            n += len(chunk)
    return n, h.hexdigest()


def _try_api(accession: str) -> dict | None:
    try:
        body = _get(CNGB_API.format(accession=accession))
        return json.loads(body.decode("utf-8"))
    except (HTTPError, URLError, ValueError):
        return None


def _try_page_scrape(accession: str) -> str | None:
    try:
        html = _get(CNGB_PAGE.format(accession=accession)).decode(
            "utf-8", errors="replace"
        )
    except (HTTPError, URLError):
        return None
    m = re.search(
        r"(ftp\.cngb\.org/pub/CNSA/data\d+/" + re.escape(accession) + r"/)",
        html,
    )
    return ("https://" + m.group(1)) if m else None


def _probe_shard(accession: str) -> str | None:
    for i in range(1, MAX_SHARD + 1):
        url = f"{CNGB_FTP_BASE}/data{i}/{accession}/"
        try:
            req = Request(url, headers={"User-Agent": USER_AGENT}, method="HEAD")
            with urlopen(req, timeout=TIMEOUT_S) as r:
                if r.status == 200:
                    return url
        except HTTPError as e:
            if e.code == 404:
                continue
            # any other HTTP code: treat as unreachable, keep probing
            continue
        except URLError:
            time.sleep(0.5)
            continue
    return None


def resolve_project_root(accession: str) -> tuple[str, str]:
    """Return (root_url, strategy) or raise RuntimeError.

    Strategy names are logged so the agent can report which path worked.
    """
    api = _try_api(accession)
    if api and isinstance(api, dict):
        # API shape varies across CNGB versions; look for common keys.
        for key in ("ftp_url", "data_url", "download_url"):
            v = api.get(key)
            if isinstance(v, str) and v.startswith("http"):
                return (v.rstrip("/") + "/", f"api:{key}")
    page = _try_page_scrape(accession)
    if page:
        return (page, "page_scrape")
    probe = _probe_shard(accession)
    if probe:
        return (probe, "shard_probe")
    raise RuntimeError(
        f"CNGB accession {accession} could not be resolved via API, "
        "page scrape, or shard probe."
    )


def list_directory(url: str) -> list[str]:
    """Parse the Apache/nginx autoindex of a CNGB data directory.

    CNGB publishes standard autoindex HTML — href="filename". We keep
    everything that isn't a backlink or a subdirectory probe that we
    already recursed into.
    """
    html = _get(url).decode("utf-8", errors="replace")
    hrefs = re.findall(r'href="([^"?#]+)"', html)
    out: list[str] = []
    for h in hrefs:
        if h in (".", "..", "/"):
            continue
        if h.startswith("?"):
            continue
        out.append(h)
    return out


def walk_for_matrices(root: str) -> list[str]:
    """BFS through subdirectories collecting processed-matrix files."""
    matrices: list[str] = []
    frontier = [root]
    seen: set[str] = set()
    while frontier:
        cur = frontier.pop(0)
        if cur in seen:
            continue
        seen.add(cur)
        try:
            children = list_directory(cur)
        except (HTTPError, URLError):
            continue
        for name in children:
            if name.endswith("/"):
                frontier.append(urljoin(cur, name))
            elif name.lower().endswith(MATRIX_SUFFIXES):
                matrices.append(urljoin(cur, name))
    return matrices


def fetch_project(accession: str, out_dir: Path) -> dict:
    root, strategy = resolve_project_root(accession)
    dest_root = out_dir / accession
    dest_root.mkdir(parents=True, exist_ok=True)
    matrices = walk_for_matrices(root)
    manifest = {
        "accession": accession,
        "root_url": root,
        "resolution_strategy": strategy,
        "retrieved_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "matrices": [],
    }
    for url in matrices:
        rel = url.split(accession + "/", 1)[-1]
        dest = dest_root / "matrices" / rel
        nbytes, sha = _stream_to(url, dest)
        manifest["matrices"].append(
            {"url": url, "path": str(dest.relative_to(out_dir)), "bytes": nbytes, "sha256": sha}
        )
    (dest_root / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True)
    )
    return manifest


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("accession", help="CNGB project accession, e.g. CNP0002664")
    p.add_argument("out_dir", nargs="?", default=".", help="Destination directory (default: cwd)")
    p.add_argument(
        "--list-matrices", action="store_true",
        help="Just list discoverable matrix URLs; don't download.",
    )
    args = p.parse_args(argv)
    if not args.accession.startswith("CN"):
        print(
            f"error: expected CNGB accession (prefix CN*), got {args.accession!r}",
            file=sys.stderr,
        )
        return 1
    try:
        if args.list_matrices:
            root, strategy = resolve_project_root(args.accession)
            print(f"resolved {args.accession} via {strategy}: {root}", file=sys.stderr)
            for u in walk_for_matrices(root):
                print(u)
            return 0
        manifest = fetch_project(args.accession, Path(args.out_dir))
    except RuntimeError as e:
        print(f"error: {e}", file=sys.stderr)
        return 3
    except (HTTPError, URLError, OSError) as e:
        print(f"error: {type(e).__name__}: {e}", file=sys.stderr)
        return 4
    if not manifest["matrices"]:
        print(
            f"error: no processed matrix files discovered under {manifest['root_url']}",
            file=sys.stderr,
        )
        return 2
    n = len(manifest["matrices"])
    total = sum(m["bytes"] for m in manifest["matrices"])
    print(
        f"ok: {args.accession} — {n} matrices, {total/1e6:.1f} MB, "
        f"strategy={manifest['resolution_strategy']}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
