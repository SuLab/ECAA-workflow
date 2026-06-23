#!/usr/bin/env python3
"""Content-addressed cache for the expensive SHACL / OWL reasoning steps.

Profiling a 33-task package shows the Turtle PARSE of the static ontology
(277 triples) + shapes (74 triples) is cheap (~40ms total); the real cost is
`pyshacl.validate` (~0.8s) and HermiT (`sync_reasoner`, ~1s). Those steps are a
pure function of (the package ABox) + (the static ontology / shapes graphs) +
(this validator's logic), so an identical (re)validation is deterministic and
can be short-circuited.

OPT-IN: caching is active only when `ECAA_SPEC_CACHE_DIR` is set, so strict
CI / `make conformance` runs that want to always re-reason simply leave it
unset (then every function here is a no-op and the validators behave and time
exactly as before). The key folds in a blank-node-invariant canonical digest of
the ABox plus a content hash of every source graph, so any change to the
ontology, the shapes, or the package automatically invalidates the entry.
"""
import hashlib
import json
import os
from pathlib import Path

# Bump when a validator's OUTPUT FORMAT or reasoning semantics change, so a
# verdict cached by an older script is never replayed against newer logic.
CACHE_VERSION = "1"


def cache_dir():
    """Cache directory from `ECAA_SPEC_CACHE_DIR`, or `None` when disabled."""
    d = os.environ.get("ECAA_SPEC_CACHE_DIR", "").strip()
    return Path(d) if d else None


def enabled():
    """True when content-addressed caching is turned on."""
    return cache_dir() is not None


def _file_digest(path):
    p = Path(path)
    if not p.exists():
        return f"absent:{p.name}"
    return hashlib.sha256(p.read_bytes()).hexdigest()


def abox_digest(graph):
    """Blank-node-invariant canonical digest of an rdflib graph.

    Uses rdflib's canonicalization so freshly-minted blank-node ids (the
    Package node's `minReaderVersion` / `specVersion` carry two) do not perturb
    the key across otherwise-identical projections.
    """
    from rdflib.compare import to_isomorphic

    return str(to_isomorphic(graph).graph_digest())


def validation_key(kind, abox_graph, source_files):
    """Stable cache key for `kind` ('shacl' | 'owl' | 'owl-static') over the
    package ABox + the static source graphs."""
    h = hashlib.sha256()
    h.update(CACHE_VERSION.encode())
    h.update(b"\x00")
    h.update(kind.encode())
    if abox_graph is not None:
        h.update(b"\x00")
        h.update(abox_digest(abox_graph).encode())
    for f in source_files:
        h.update(b"\x00")
        h.update(_file_digest(f).encode())
    return h.hexdigest()


def lookup(key):
    """Return cached `{exit_code, output}` for `key`, or `None` on miss /
    disabled / corrupt entry."""
    d = cache_dir()
    if d is None or key is None:
        return None
    p = d / f"{key}.json"
    if not p.exists():
        return None
    try:
        rec = json.loads(p.read_text())
    except Exception:
        return None
    if isinstance(rec, dict) and "exit_code" in rec and "output" in rec:
        return rec
    return None


def store(key, exit_code, output):
    """Persist `{exit_code, output}` for `key` via atomic temp+rename. No-op
    when caching is off. Best-effort: a write failure must never break a
    validation, so all errors are swallowed."""
    d = cache_dir()
    if d is None or key is None:
        return
    try:
        d.mkdir(parents=True, exist_ok=True)
        tmp = d / f"{key}.json.tmp.{os.getpid()}"
        tmp.write_text(json.dumps({"exit_code": int(exit_code), "output": output}))
        tmp.replace(d / f"{key}.json")
    except Exception:
        pass
