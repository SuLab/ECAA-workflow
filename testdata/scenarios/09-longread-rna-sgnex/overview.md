# SG-NEx Nanopore Long-Read RNA-Seq Isoform Discovery and Differential Transcript Usage

## Project goal
Re-derive transcript-level abundance, isoform discovery, and differential transcript usage (DTU) across human cell lines from the Singapore Nanopore Expression Project (SG-NEx) benchmark (Chen 2025 *Nature Methods*). The deliverable is a per-tool isoform-level precision/recall table against the SG-NEx spike-in and sequin truth reference, and a cross-cell-line DTU table for the seven core cell lines.

## Strategy
Ingest the SG-NEx core benchmark (7 cell lines × 5 library protocols) from ENA BioProject **PRJEB44348** or from the AWS Open Data registry mirror (`sgnex`) or the `GoekeLab/sg-nex-data` GitHub index. Align direct-RNA, direct-cDNA, PCR-cDNA, Illumina cDNA, and PacBio IsoSeq reads to GRCh38 + GENCODE with minimap2 (splice preset for Nanopore, asm20 for PacBio), and quantify isoforms with three independent callers: Bambu (Chen 2023 *Nat Methods*), FLAIR (Tang 2020 *Nat Commun*), and IsoQuant (Prjibelski 2023 *Nat Biotech*). Evaluate isoform-discovery precision/recall against the SG-NEx sequin/SIRV spike-in truth set. Run differential transcript usage between cell lines with DRIMSeq or StageR, and compare Nanopore-based DTU calls against the matched Illumina-derived DTU from the SG-NEx short-read arm.

## Key challenges
- **Error profile:** Nanopore direct-RNA reads have ~5–10% per-base error with systematic homopolymer bias; alignments must handle soft-clipping and indel-rich regions without losing true isoform boundaries.
- **Protocol heterogeneity:** direct RNA, direct cDNA, PCR cDNA, and PacBio IsoSeq have very different error profiles, chimera rates, and sequencing-depth distributions. The package must report per-protocol results separately and NOT pool across protocols in the primary analysis.
- **Truth set definition:** the SG-NEx benchmark defines truth using spike-in controls (sequins, SIRVs) rather than gold-standard transcript annotation. Isoform discovery precision/recall MUST be reported against the spike-ins, not against GENCODE.
- **Differential transcript usage vs differential expression:** DTU is a distinct hypothesis from DTE (differential transcript expression) and DGE (differential gene expression); the package must make this distinction explicit and report all three only if all three are requested.
- **Reference annotation drift:** GENCODE is updated frequently; choosing a different GENCODE release between the alignment step and the annotation step silently changes isoform IDs. The package must pin the GENCODE release.

## Work completed so far
SG-NEx is the community-standard benchmark for Nanopore long-read RNA-seq isoform analysis and has been used to parameterize every long-read isoform caller published since 2020. Chen 2025 *Nat Methods* provides the reference per-tool evaluation. A rerunnable end-to-end package that pins the caller versions, the GENCODE release, and the spike-in truth tables, and emits a fail-closed gate on precision/recall floors, is still absent from most internal pipelines.

## Core methodological question
Whether each of Bambu, FLAIR, and IsoQuant recovers the SG-NEx sequin/SIRV spike-in isoforms at ≥ 0.80 recall and ≥ 0.75 precision under direct-RNA reads, and whether Nanopore-based cross-cell-line DTU calls agree with Illumina-based DTU calls at a prespecified Jaccard floor. If either criterion fails, the package must stop and refuse to emit isoform-level claims.

## Potential analysis prompts
1. Download SG-NEx core benchmark (7 cell lines × 5 protocols) from ENA PRJEB44348 or AWS Open Data `sgnex`.
2. Align direct-RNA, direct-cDNA, PCR-cDNA with minimap2 `-ax splice -uf -k14` (for direct-RNA) or `-ax splice` (for cDNA).
3. Align PacBio IsoSeq with minimap2 `-ax splice:hq`.
4. Run Bambu with GENCODE annotation (pinned release).
5. Run FLAIR with the same annotation.
6. Run IsoQuant with the same annotation.
7. Benchmark each caller against SG-NEx sequin/SIRV spike-in truth.
8. Differential transcript usage across cell-line contrasts with DRIMSeq + StageR.
9. Compare Nanopore DTU against matched Illumina-based DTU using the SG-NEx short-read arm and Salmon + DRIMSeq.

## Conservative claim boundaries
Descriptive per-tool isoform-discovery benchmarks and cross-cell-line DTU calls only. No disease, tissue-specific, or functional claims. Isoform-level annotation extensions are hypothesis-generating until confirmed by an independent assay.

## References
- Chen Y, Davidson NM, Wan YK, et al. 2025. A systematic benchmark of Nanopore long-read RNA sequencing for transcript-level analysis in human cell lines (SG-NEx). *Nat Methods* 22:801–812. DOI 10.1038/s41592-025-02623-4. PMID 40082608.
- Li H. 2018. Minimap2: pairwise alignment for nucleotide sequences. *Bioinformatics* 34:3094–3100. DOI 10.1093/bioinformatics/bty191.
- Chen Y, Sim A, Wan YK, et al. 2023. Context-aware transcript quantification from long-read RNA-seq data with Bambu. *Nat Methods* 20:1187–1195. DOI 10.1038/s41592-023-01908-w.
- Tang AD, Soulette CM, van Baren MJ, et al. 2020. Full-length transcript characterization of SF3B1 mutation in chronic lymphocytic leukemia reveals downregulation of retained introns (FLAIR). *Nat Commun* 11:1438. DOI 10.1038/s41467-020-15171-6.
- Prjibelski AD, Mikheenko A, Joglekar A, et al. 2023. Accurate isoform discovery with IsoQuant using long reads. *Nat Biotechnol* 41:915–918. DOI 10.1038/s41587-022-01565-y.
- Kovaka S, Zimin AV, Pertea GM, Razaghi R, Salzberg SL, Pertea M. 2019. Transcriptome assembly from long-read RNA-seq alignments with StringTie2. *Genome Biol* 20:278. DOI 10.1186/s13059-019-1910-1.
- Patro R, Duggal G, Love MI, Irizarry RA, Kingsford C. 2017. Salmon provides fast and bias-aware quantification of transcript expression. *Nat Methods* 14:417–419.
