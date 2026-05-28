# Workflow Context

**Modality:** variant_calling
**Domain:** computational biology
**Description:** Germline short-variant (SNV / indel) calling from WGS or WES. Standard
pipeline: raw QC, trim, align, call variants, filter, annotate. Somatic
/ tumor-normal variants are out of scope; a separate archetype will
cover that workload. Mirrors today's
`config/modalities/variant-calling.yaml` + `config/archetypes/`.

**EDAM topic:** topic:3673
**EDAM operation:** operation:3227
**Confidence:** high (100%)

## Organisms
- Homo sapiens (taxon:9606)

## Methods mentioned in SME prose

_Keyword-scraped from intake text; see SME discovery decisions above for authoritative values._

- variant_calling: GATK HaplotypeCaller
- variant_calling: DeepVariant

## SME intake text

We need to set up a Mendelian rare-disease variant prioritization benchmark using fully public data. The patient construct is the Genome in a Bottle Ashkenazi Jewish trio: HG002 as the proband, with HG003 (father) and HG004 (mother). All three are deposited under the NIST GIAB project on AWS Open Data and on the GIAB FTP. Standard 30-fold whole-genome sequencing per individual, GRCh38 reference.

For the prioritization-evaluation construct, we will spike a pinned set of nine ClinVar pathogenic single-nucleotide and short-indel variants into HG002 -- three autosomal recessive, three autosomal dominant de novo, and three X-linked recessive -- drawn from the companion file research/paper-recreation/data/plan-12-spikein-variants.tsv. Each spiked variant is tagged with a representative HPO term cluster from the disease's entry in OMIM / Orphanet, simulating the patient's phenotype.

The pipeline runs the standard germline trio calling: BWA-MEM2 or DeepVariant alignment, GATK HaplotypeCaller GVCF per sample, joint genotyping with the pedigree file, VQSR or hard filters, and a Mendelian-error de novo filter. Variants are annotated for consequence with VEP, intersected with the GnomAD population frequency database, and prioritized using an HPO-driven scoring tool -- Exomiser or equivalent. Output is the top-50 ranked candidate list per inheritance mode.

Acceptance: (a) the trio-relatedness check confirms kinship at the expected parent-child coefficient before any variant analysis runs; (b) GIAB germline recall against the v4.2.1 truth set in HG002 hits >= 0.99 SNV (typical) and >= 0.92 indel (HaplotypeCaller-typical; >= 0.96 with DeepVariant), benchmarked through hap.py per Krusche 2019; (c) at least 95% of the spiked pathogenic variants appear in the top-50 prioritized list when the matching HPO terms are supplied.

Claim boundary: variant detection + ranking only. No clinical diagnosis, no treatment recommendation. The HPO score is a ranking heuristic, not a clinical interpretation.

