# Workflow Context

**Modality:** chip_seq
**Domain:** computational biology
**Description:** ChIP-seq peak calling + peak-vs-input enrichment. Standard pipeline:
raw QC, trim, align, call peaks against the matched input control, then
report. Mirrors today's `config/modalities/chip-seq.yaml` + `config/archetypes/`.

**EDAM topic:** topic:3169
**EDAM operation:** operation:3222
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- alignment: BWA-MEM
- peak_calling: MACS2

## SME intake text

I'd like to re-run the canonical ENCODE Transcription Factor ChIP-seq uniform pipeline on the CTCF K562 reference experiment. The accession at the ENCODE portal is ENCSR000DWE, which has two biological replicates plus a matched input chromatin control. Reads are 36 bp single-end Illumina. Reference genome is GRCh38 with the ENCODE pipeline reference TSV.

The pipeline I want to follow is the ENCODE chip-seq-pipeline2 conventions exactly: BWA aln/samse for alignment with the ENCODE parameter set, Picard MarkDuplicates with duplicates removed, MAPQ >= 30 filter, ENCODE blacklist v2 (ENCFF356LFX) subtracted, phantompeakqualtools (run_spp.R) for cross-correlation NSC/RSC, MACS2 callpeak in narrow mode using the spp-derived fragment length and per-replicate matched input, then IDR with a soft threshold of 0.05 on rep1-vs-rep2 ranked by signalValue.

The output I want is a IDR-thresholded narrowPeak BED, plus the standard QC table: usable read count per replicate, NSC, RSC, NRF, PBC1, PBC2, and FRiP. Then I want a head-to-head comparison against the GRCh38 default-analysis released file ENCFF843VHC (the preferred_default=True optimal IDR-thresholded narrowPeak on the experiment page; the hg19 equivalent is ENCFF002DDJ) -- bedtools jaccard overlap and a peak-count delta.

For motif validation, run MEME-ChIP or HOMER findMotifsGenome.pl on the top 500 peaks by signalValue and confirm that the JASPAR CTCF motif (MA0139.1) is the top de novo discovered motif at E < 1e-100.

Acceptance: peak count within +/-15% of the released file, Jaccard overlap >= 0.85, NSC >= 1.05, RSC >= 0.8, FRiP >= 0.05, top motif is CTCF.

