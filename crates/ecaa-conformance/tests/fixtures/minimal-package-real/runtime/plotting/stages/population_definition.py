"""Population definition stage — clinical-trial taxonomy. Renders the
CONSORT participant-flow diagram from the manifest's
``population_flow`` dict, mirroring the SAP's pre-specified analysis
sets (ITT / mITT / per-protocol).

Plan reference: §S12 (clinical-trial-analysis taxonomy stage
`population_definition`).

Manifest contract:
- ``population_flow``: dict mapping the five required CONSORT stages
  (``enrolled``, ``randomized``, ``allocated``, ``followed_up``,
  ``analyzed``) to counts, plus optional ``<stage>_excluded`` siblings
  carrying the exclusion reason text.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, Optional

from ..core import (
    FigureContext,
    consort_diagram,
    register_figure,
    stage_registry,
)

FIGURES = stage_registry("population_definition")


@register_figure(FIGURES, "consort_diagram")
def consort_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    flow: Optional[Dict[str, Any]] = ctx.manifest.get("population_flow")
    if not isinstance(flow, dict) or not flow:
        raise FileNotFoundError("manifest.population_flow required")
    return consort_diagram(flow=flow, title="Participant flow", out=out)
