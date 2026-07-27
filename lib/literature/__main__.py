"""Package-portable entrypoint for the shipped literature library.

Runnable inside any emitted package as

    python3 -m runtime.literature \\
        --results runtime/outputs/<stage>/<table> \\
        --prior-claims runtime/outputs/review_prior_work/prior_claims_matrix.csv \\
        --evidence-manifest runtime/outputs/review_prior_work/evidence/manifest.json \\
        --out-dir runtime/outputs/contextualize_findings_with_literature

It delegates through a RELATIVE import, so the identical module works under
any parent package name (`runtime.literature` in a shipped package,
`lib.literature` in the repo) without a path rewrite. Running the submodule
directly (`-m runtime.literature.contextualize`) also works but re-executes
a module the package `__init__` already imported, which emits a runpy
warning — prefer this entrypoint.

The summary is emitted as JSON on STDOUT so the agent wrapper can fold it
into `result.json`.

Exit codes:
  0 — the matrix and its companion artifacts were written.
  2 — argument error (argparse).
"""

from __future__ import annotations

import sys

from .contextualize import main

if __name__ == "__main__":
    sys.exit(main())
