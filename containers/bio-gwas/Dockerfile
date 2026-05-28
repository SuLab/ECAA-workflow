# bio-gwas — GWAS / fine-mapping / colocalization tools, layered on
# top of bio-min. Per containers/README.md option-3 layered model.
#
# Build:
#   scripts/build-bio-domain.sh bio-gwas v0.1.0
#
# Used by atoms (Stage 15 follow-on): gwas_summary_harmonization,
# colocalization, population_definition (when overlap-based).

ARG BIO_MIN_TAG=v0.1.0-eval
FROM ghcr.io/scripps/bio-min:${BIO_MIN_TAG} AS base

USER root

# PLINK 1.9 + 2.0 (binary-only releases; pinned by checksum). Kept
# under /opt/plink so the bio-min `bio` user can read but not write.
ARG PLINK1_VERSION=20240526
# F-DEP-L-05: pin sha256 against the S3 release asset (resolved 2026-05-14).
ARG PLINK1_SHA256=e04a949e46415c0f2827d7cf58767bdabd17807c0d5b605628a3783cd6d2918c
RUN curl -fsSL -o /tmp/plink1.zip \
        "https://s3.amazonaws.com/plink1-assets/plink_linux_x86_64_${PLINK1_VERSION}.zip" \
 && echo "${PLINK1_SHA256}  /tmp/plink1.zip" | sha256sum -c - \
 && mkdir -p /opt/plink1 \
 && unzip -d /opt/plink1 /tmp/plink1.zip \
 && rm /tmp/plink1.zip \
 && ln -s /opt/plink1/plink /usr/local/bin/plink

ARG PLINK2_VERSION=20240625
# F-DEP-L-05: pin sha256 against the S3 release asset (resolved 2026-05-14).
ARG PLINK2_SHA256=d7da1e57e869de394792338aea16ce66c0ba093b4c8ce850ccb6f8bb6e8e1ca6
RUN curl -fsSL -o /tmp/plink2.zip \
        "https://s3.amazonaws.com/plink2-assets/plink2_linux_x86_64_${PLINK2_VERSION}.zip" \
 && echo "${PLINK2_SHA256}  /tmp/plink2.zip" | sha256sum -c - \
 && mkdir -p /opt/plink2 \
 && unzip -d /opt/plink2 /tmp/plink2.zip \
 && rm /tmp/plink2.zip \
 && ln -s /opt/plink2/plink2 /usr/local/bin/plink2

# regenie via bioconda — fast whole-genome regression. coloc + susie
# come from R Bioconductor.
RUN mamba install -n base -y \
        regenie=3.6 \
        bcftools=1.20 \
        tabix=1.20 \
        bioconductor-coloc=5.2.* \
        bioconductor-susier=0.12.* \
        r-tidyverse=2.0.* \
 && mamba clean --all -f -y

# Python GWAS support (PyVCF, pgsc_calc, statsgenetics, fine-mapping).
RUN python -m pip install --no-cache-dir \
        "pgenlib>=0.91" \
        "scikit-allel>=1.3" \
        "pyranges>=0.0.129"

# Record per-domain additions on top of bio-min for the
# RUNTIME_VERSIONS contract (Stage 15 / SWFC_AMI_RUNTIME_STRICT).
RUN mkdir -p /etc/bio-gwas \
 && python -c 'import json,subprocess; \
    raw = subprocess.check_output(["mamba","list","--json"]).decode(); \
    items = json.loads(raw); \
    keep = ["regenie","bcftools","tabix","bioconductor-coloc","bioconductor-susier"]; \
    out = {p["name"]: p["version"] for p in items if p["name"] in keep}; \
    out["plink1"] = "'"${PLINK1_VERSION}"'"; \
    out["plink2"] = "'"${PLINK2_VERSION}"'"; \
    out["base_image"] = "ghcr.io/scripps/bio-min:'"${BIO_MIN_TAG}"'"; \
    print(json.dumps(out, indent=2))' > /etc/bio-gwas/RUNTIME_VERSIONS.json

USER bio:bio
WORKDIR /work

ENTRYPOINT ["/bin/bash"]
