# Germline Whole-Genome Variant Calling Benchmarked Against GIAB Truth Sets

## Project goal
Deliver a reproducible, fail-closed benchmarking package for germline short-variant calling on human whole-genome sequencing data, using the NIST Genome in a Bottle (GIAB) reference samples as the ground truth. The package produces per-caller precision and recall for SNVs and small indels against the GIAB high-confidence truth VCF + high-confidence BED, stratified by genomic region difficulty.

## Strategy
Align raw FASTQ from the GIAB reference individuals (NA12878/HG001, Ashkenazi trio HG002/HG003/HG004) to GRCh38 using BWA-MEM2, mark duplicates, and call germline SNVs and indels with both GATK4 HaplotypeCaller and Google DeepVariant. Compare the output VCFs to the GIAB v4.2.1 truth set using `hap.py` + `vcfeval` engine (RTG Tools) inside the high-confidence BED region, and compute precision / recall / F1 for SNVs and indels separately. Stratify results into GIAB-defined difficulty regions (segmental duplications, homopolymers, GC extremes, tandem repeats, low-mappability) using the official stratification BED files from NIST.

## Key challenges
- **Data volume:** each 30× WGS BAM is ~60 GB; the package must manage runtime I/O carefully and stream-align rather than materialize intermediate artifacts where possible.
- **Reference equivocation:** truth sets differ between GRCh37 and GRCh38; the package must pin a single reference build per run and forbid cross-build comparisons.
- **Caller conventions:** GATK and DeepVariant emit different quality score semantics and different multi-allelic representation; hap.py requires normalization (`bcftools norm --multiallelics -`) and left-alignment before comparison.
- **Stratification correctness:** the GIAB stratification BEDs must match the truth-set version (v4.2.1) and reference build exactly; using a mismatched stratification set silently corrupts the precision/recall numbers.
- **Dataset drift:** the GIAB truth set itself is versioned (v3.3.2 → v4.0 → v4.2.1 → draft v4.3 using T2T), and results are not directly comparable across versions. The package must pin and log the truth-set version.

## Work completed so far
GIAB has become the de-facto standard for germline caller benchmarking (Zook 2019 *Nat Biotechnol*, Krusche 2019 *Nat Biotechnol* hap.py best-practices). The `germline-benchmark` community scripts and the PrecisionFDA Truth Challenges (v1 2016, v2 2020, V2 phase 2) establish reference runs for GATK, DeepVariant, Strelka2, Octopus, Clair3 and others. A rerunnable, fail-closed Rust-or-Nextflow packaging that ingests a fresh BAM and emits both the hap.py precision/recall table and the stratified breakdown with explicit provenance records is still absent from most internal pipelines.

## Core methodological question
Whether DeepVariant outperforms GATK HaplotypeCaller on a 30× Illumina short-read WGS sample of HG002 within the GIAB v4.2.1 high-confidence region, across SNV and indel categories and across difficulty strata. The answer is already known in aggregate from PrecisionFDA (DeepVariant is typically ≥0.99 F1 for SNVs and ~0.99 for indels, GATK ~0.99 SNV and ~0.98 indel), but the package's job is to reproduce those numbers from raw data end-to-end with bitwise provenance.

## Potential analysis prompts
1. Align a GIAB HG002 30× NovaSeq BAM/FASTQ to GRCh38 with BWA-MEM2.
2. Mark duplicates with Picard or `samtools markdup`.
3. Call variants with GATK4 HaplotypeCaller (GVCF → GenotypeGVCFs).
4. Call variants in parallel with DeepVariant (WGS model).
5. Normalize both VCFs against GRCh38 reference with `bcftools norm`.
6. Run hap.py + vcfeval against GIAB v4.2.1 truth + high-confidence BED.
7. Stratify precision/recall by GIAB difficulty BEDs.
8. Optional: call with Strelka2 or Clair3 as a third arm.
9. Optional: run the full Ashkenazi trio (HG002/HG003/HG004) and verify Mendelian consistency of trio-joint-genotyped calls.

## Conservative claim boundaries
Descriptive caller precision/recall on a reference benchmark only. No claims about clinical performance, diagnostic suitability, or caller superiority beyond the region stratifications actually measured. Results are not extrapolated to non-Illumina technologies.

## References
- Zook JM, McDaniel J, Olson ND, et al. 2019. An open resource for accurately benchmarking small variant and reference calls. *Nat Biotechnol* 37:561–566. DOI 10.1038/s41587-019-0074-6. PMID 30936564.
- Krusche P, Trigg L, Boutros PC, et al. 2019. Best practices for benchmarking germline small-variant calls in human genomes. *Nat Biotechnol* 37:555–560. DOI 10.1038/s41587-019-0054-x.
- Poplin R, Ruano-Rubio V, DePristo MA, et al. 2018. Scaling accurate genetic variant discovery to tens of thousands of samples. *bioRxiv* 201178. DOI 10.1101/201178.
- Poplin R, Chang PC, Alexander D, et al. 2018. A universal SNP and small-indel variant caller using deep neural networks. *Nat Biotechnol* 36:983–987. DOI 10.1038/nbt.4235.
- Li H. 2013. Aligning sequence reads, clone sequences and assembly contigs with BWA-MEM. *arXiv* 1303.3997.
- Van der Auwera GA, O'Connor BD. 2020. *Genomics in the Cloud*. O'Reilly. ISBN 978-1-491-97519-0.
- Byrska-Bishop M, Evani US, Zhao X, et al. 2022. High-coverage whole-genome sequencing of the expanded 1000 Genomes Project cohort including 602 trios. *Cell* 185:3426–3440.e19. DOI 10.1016/j.cell.2022.08.004.
- GIAB FTP: https://ftp-trace.ncbi.nlm.nih.gov/ReferenceSamples/giab/release/
- Illumina hap.py: https://github.com/Illumina/hap.py
