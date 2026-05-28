"""Pytest hooks for the plotting test suite.

`--update-hashes` flag for the snapshot tests lives here so pytest picks
it up at collection time (test modules can't add CLI flags).
"""

from __future__ import annotations


def pytest_addoption(parser):
    parser.addoption(
        "--update-hashes",
        action="store_true",
        default=False,
        help="overwrite snapshot-hashes.json with the current renders",
    )
