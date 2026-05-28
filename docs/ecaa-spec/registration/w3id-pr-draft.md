# w3id.org redirect registration — ECAA v0.1

This file is the source text of a pull request to be submitted to https://github.com/perma-id/w3id.org to register the `ecaa/` and `ecaa/v0.1` permanent IRIs.

## Target files to add in the perma-id/w3id.org repository

```
ecaa/.htaccess
ecaa/index.html
ecaa/v0.1/.htaccess
ecaa/v0.1/index.html
```

## `ecaa/.htaccess` content

```apache
RewriteEngine On
RewriteBase /ecaa
RewriteRule ^$ https://github.com/scripps-workflow/awa-workflow/tree/main/docs/ecaa-spec [R=303,L]
RewriteRule ^v0.1$ https://github.com/scripps-workflow/awa-workflow/blob/main/docs/ecaa-spec/v0.1.md [R=303,L]
RewriteRule ^ns/0.1#?(.*)$ https://w3id.org/ecaa/ns/0.1 [R=303,L]
```

## PR body template

```markdown
This PR registers permanent IRIs for the **Evidence-Carrying Analysis Artifact (ECAA)** specification, a published bioinformatics analysis-package conformance contract.

- Specification: https://github.com/scripps-workflow/awa-workflow/blob/main/docs/ecaa-spec/v0.1.md
- Reference implementation: https://github.com/scripps-workflow/awa-workflow
- Authority: PAR-26-040 / NIH NLM R01 grant (PD/PI alan@scripps).
- Open-source license: Apache-2.0.
- Domain: bioinformatics analysis provenance + machine-checkable claim-to-evidence binding.

Registered IRIs:
- `https://w3id.org/ecaa/` — current spec version index
- `https://w3id.org/ecaa/v0.1` — version 0.1 spec document
- `https://w3id.org/ecaa/ns/0.1#` — OWL namespace for the v0.1 ontology

Cc: @<community reviewers, if applicable>
```

## Action items

1. Fork `perma-id/w3id.org` on GitHub.
2. Create the four files above in the fork (matching the apache configurations w3id uses for similar prefixes — see existing `ro-crate/`, `bridi/`, `idsa/` examples for the pattern).
3. Submit the PR with the body template above.
4. Once merged, verify `https://w3id.org/ecaa/v0.1` redirects to the live spec document.
5. Record the merged-PR link in `docs/ecaa-spec/registration/w3id-pr-merged.md` for future provenance.
