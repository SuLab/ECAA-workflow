# PacBio HiFi CRISPR Repair-Scar Amplicon Package Request

Create a fully autonomous-ready internal research package for a PacBio HiFi long-read
amplicon readout of CRISPR editing outcomes at an engineered knock-in construct locus.

Primary objective: characterize the on-target repair scar carried by each edited HiFi
read at the construct locus — which reads are on-target integrations, and what
structural scar (inversion, deletion, insertion, or clean HDR) each carries — and
report a per-read repair-scar table with construct-, segment-map-, and reference-scoped
provenance.

Context:

- Amplicon libraries are multiplexed across samples using PacBio barcoded adapters
  (Lima-compatible barcode set); the package must demultiplex by sample barcode before
  any per-sample QC.
- Reads must pass a structural-feature gate keyed to the construct's segment map
  (5' homology arm, payload, 3' homology arm, and flanking genomic segments) before
  alignment — reads whose segment-track topology does not satisfy the retention
  predicate are excluded and reported as "did not match", not "unedited".
- A fraction of on-target-looking reads are expected to be human-genome-identical
  (i.e., indistinguishable from unmodified endogenous sequence at >99% identity against
  the human reference/BLAST database) and must be removed as human-identical prior to
  alignment against the construct — these are reported as "human-identical", never as
  "unedited" or "wild-type".
- Surviving reads are aligned to the construct reference with `pbmm2` (long-read-aware
  minimap2 wrapper); reverse-complement (`_INV`-flagged) alignments indicate a candidate
  inversion event and require orthogonal confirmation before being reported as validated.
- Per-read repair-scar characterization must report segment-track order, jump type
  (insertion/deletion/inversion/none), and track length per read, scoped explicitly to
  the supplied construct reference version, segment map version, and human
  reference/BLAST DB version.

Data availability and scope:

- All sequencing data are internally generated PacBio HiFi amplicon reads from an
  engineered cell line; no public accession is required for the anchor dataset.
- No controlled-access, PHI, or human subjects data is used — the human reference is
  used only as a contamination filter, not as a subject-identifiable dataset.
- Treat this as a single-locus, single-construct amplicon editing-outcome project.
  Genome-wide off-target profiling (e.g. GUIDE-seq, CIRCLE-seq) and pooled CRISPR
  screens are explicitly out of scope for this package.
- Publication scope: internal only.
- Governance: internally generated research data only; no PHI, no controlled-access
  data, no secrets.

Required package behavior:

- The package must be suitable for downstream autonomous execution after generation.
- Explicit control decisions required for: (a) barcode/demultiplexing tool and
  mismatch tolerance, (b) the structural retention predicate applied against the
  segment map, (c) the human-identity threshold used to call a read "human-identical"
  (default >99%), (d) the `pbmm2` alignment preset against the construct reference, and
  (e) fail-closed stop conditions when the construct reference, segment map, or human
  reference/BLAST DB versions are unpinned or mismatched between runs.
- Conservative claim boundaries: every count and scar description is scoped to the
  exact construct reference, segment map, and human reference/BLAST DB versions used in
  that run — report them alongside every count. Reads removed by the structural gate or
  the host-contamination filter are reported as "did not match" / "human-identical",
  never as "unedited". An `_INV`-flagged alignment is a candidate inversion only; a
  validated inversion event requires orthogonal (e.g. long-range PCR or orthogonal
  sequencing) confirmation. No claims about editing efficiency generalizing beyond this
  construct/locus, and no clinical or therapeutic-relevance claims.
- If runtime refinement occurs (e.g. adjusting the structural retention predicate), the
  provenance and rationale must be logged explicitly.
- Literature grounding: at minimum a PacBio HiFi/CCS methods reference, a `pbmm2`/
  minimap2 alignment reference (Li 2018 *Bioinformatics*), and a CRISPR repair-outcome
  characterization reference (e.g. large deletion / structural-variant repair-outcome
  literature) must appear in the package bibliography.

## Extracted Overview Preview

```
Project goal: per-read repair-scar characterization from PacBio HiFi amplicon data
at a single engineered CRISPR knock-in construct locus.
Pipeline: acquire reads -> demultiplex by sample barcode -> per-sample QC ->
structural-feature read gate (segment map) -> human-contamination removal
(>99% identity) -> pbmm2 alignment to construct reference -> per-read repair-scar
characterization (segment track, jump type, track length) -> reporting.
Claim boundaries: scar descriptions scoped to construct reference + segment map +
human reference/BLAST DB versions; no unedited/wild-type labels for filtered reads;
_INV reverse-complement alignments are candidate inversions only.
```
