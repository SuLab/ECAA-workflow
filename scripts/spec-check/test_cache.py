"""Tests for the content-addressed SHACL/OWL result cache (`_cache.py`).

Run: `python3 -m pytest scripts/spec-check/test_cache.py -q`
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import _cache  # noqa: E402
from rdflib import BNode, Graph, Namespace, URIRef  # noqa: E402

EX = Namespace("http://example.org/")


def _graph(n):
    g = Graph()
    for i in range(n):
        g.add((URIRef(f"http://example.org/s{i}"), EX.p, URIRef(f"http://example.org/o{i}")))
    return g


def test_disabled_by_default(monkeypatch):
    monkeypatch.delenv("ECAA_SPEC_CACHE_DIR", raising=False)
    assert _cache.enabled() is False
    assert _cache.cache_dir() is None
    assert _cache.lookup("anykey") is None
    _cache.store("anykey", 0, "x")  # no-op, must not raise


def test_store_lookup_roundtrip(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_SPEC_CACHE_DIR", str(tmp_path))
    assert _cache.enabled() is True
    g = _graph(5)
    src = tmp_path / "ont.ttl"
    src.write_text("# ont v1")
    key = _cache.validation_key("shacl", g, [src])
    assert _cache.lookup(key) is None  # cold
    _cache.store(key, 0, "VERDICT-A\n")
    assert _cache.lookup(key) == {"exit_code": 0, "output": "VERDICT-A\n"}


def test_key_is_stable_and_input_sensitive(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_SPEC_CACHE_DIR", str(tmp_path))
    src = tmp_path / "ont.ttl"
    src.write_text("# ont v1")
    base = _cache.validation_key("shacl", _graph(5), [src])

    # identical inputs -> identical key (a cache HIT)
    assert _cache.validation_key("shacl", _graph(5), [src]) == base
    # a different ABox -> different key (MISS)
    assert _cache.validation_key("shacl", _graph(6), [src]) != base
    # a different kind -> different key (SHACL vs OWL never collide)
    assert _cache.validation_key("owl", _graph(5), [src]) != base
    # a changed source file -> different key (ontology/shapes edit invalidates)
    src.write_text("# ont v2")
    assert _cache.validation_key("shacl", _graph(5), [src]) != base


def test_version_bump_invalidates(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_SPEC_CACHE_DIR", str(tmp_path))
    g = _graph(3)
    src = tmp_path / "s.ttl"
    src.write_text("x")
    before = _cache.validation_key("shacl", g, [src])
    monkeypatch.setattr(_cache, "CACHE_VERSION", _cache.CACHE_VERSION + "-bumped")
    assert _cache.validation_key("shacl", g, [src]) != before


def test_blank_node_invariant_key(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_SPEC_CACHE_DIR", str(tmp_path))

    def with_fresh_bnode():
        g = Graph()
        b = BNode()  # a new random id each call
        g.add((URIRef("http://example.org/s"), EX.p, b))
        g.add((b, EX.q, URIRef("http://example.org/o")))
        return g

    # Two graphs that differ only in blank-node ids must hash to the SAME key,
    # so a re-projected package (fresh BNode ids) still HITs the cache.
    assert _cache.validation_key("shacl", with_fresh_bnode(), []) == _cache.validation_key(
        "shacl", with_fresh_bnode(), []
    )


def test_owl_static_key_has_no_abox(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_SPEC_CACHE_DIR", str(tmp_path))
    src = tmp_path / "ont.ttl"
    src.write_text("# ont")
    # static OWL check (no package) passes abox_graph=None and must not raise.
    key = _cache.validation_key("owl-static", None, [src])
    assert isinstance(key, str) and len(key) == 64
