"""pytest bootstrap for the eval suite.

The test modules import their targets as ``from scripts.eval.<mod> import ...``,
which requires the REPO ROOT (the parent of ``scripts/``) on ``sys.path`` so the
top-level ``scripts`` namespace package resolves. When the suite is invoked from
the repo root pytest inserts the rootdir automatically; when it is invoked from
``scripts/eval`` (the documented ``cd scripts/eval && python3 -m pytest tests``
command) the rootdir is ``scripts/eval`` and ``scripts`` is unreachable, so every
test module fails to collect with ``ModuleNotFoundError: No module named
'scripts'``.

Prepending the repo root here makes the documented verify command work from any
cwd without changing how the modules import.
"""
import sys
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[2]
if str(_REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(_REPO_ROOT))
