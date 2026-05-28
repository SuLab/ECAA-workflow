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

- alignment: Bowtie2
- variant_calling: GATK HaplotypeCaller
- variant_calling: DeepVariant
- variant_calling: Mutect2

## Data sources
- PRJNA496703 (NCBI BioProject)
- SRP162370 (NCBI SRA Project)

## SME intake text

We want to recreate the SEQC2 somatic-mutation truth-set construction pipeline from Fang et al. 2019 (bioRxiv) / 2021 (Nat Biotechnol). The reference samples are HCC1395 (triple-negative breast cancer) and HCC1395BL (matched B-lymphocyte normal), with raw FASTQs on SRA under SRP162370 (BioProject PRJNA496703) totalling 140 billion reads.

The pipeline is a multi-aligner x multi-caller cross product. Run three aligners on the same FASTQs: BWA-MEM (default), Bowtie2, and NovoAlign. Run six somatic callers against each tumor/normal alignment: MuTect2, SomaticSniper, VarDict, MuSE, Strelka2, and TNscope. Per-data-group SomaticSeq machine-learning classifier (Fang 2015) labels each call PASS or REJECT.

The super set is the union of PASS calls. Confidence levels are assigned by the cross-caller agreement pattern: HighConf = many PASS / few REJECT; MedConf = some PASS / few REJECT, mostly low VAF; LowConf = few PASS at or below the 50x detection limit; Unclassified = significant REJECT (poor mapping / poor alignment / germline risk).

Validate the truth set with three orthogonal platforms: AmpliSeq deep MiSeq at 2000x depth, Ion Torrent WES at 34x, and HiSeq WES at 12,500x. Compute Pearson correlation between Super-Set VAF and validation-platform VAF, restricted to HighConf.

Headline numbers we want to reproduce (within +/-5%): 37,094 SNV HighConf, 1,588 INDEL HighConf, 2,442 SNV MedConf, 8,143 SNV LowConf, 62,407 SNV Unclassified. Validation rate for AmpliSeq HighConf SNV >= 98.7%. HiSeq WES HighConf SNV validation rate = 100%. Pearson R >= 0.95 between Super-Set VAF and AmpliSeq VAF for HighConf.

For the germline arm, joint-genotype across 63 BAMs with FreeBayes, RTG, DeepVariant, and GATK HaplotypeCaller and assign an SNV Call Probability (SCP) score per call from cross-caller agreement. Reproduce 3,597,940 SCP=1 germline calls and the AmpliSeq germline VAF correlation (R ~ 0.986).

Reference genome is GRCh38. Exclude chr6p, chr16q, and chrX from the somatic truth set due to losses in HCC1395BL. VAF detection floor is 5%; calls below this are blacklisted from benchmarking.

No clinical-utility claims -- this is the reference truth-set construction.

