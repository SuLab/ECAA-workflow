# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| main (rolling) | ✅ |
| Tagged releases ≤ 6 months old | ✅ |
| Tagged releases > 6 months old | ❌ |

## Reporting a Vulnerability

Report security issues privately via a GitHub Security Advisory at `https://github.com/SuLab/ECAA-workflow/security/advisories/new` (update this URL after the first remote push).

Please **do not** open public GitHub issues for security vulnerabilities.

### Response SLA

| Stage | Maximum time |
|---|---|
| Initial acknowledgment | 72 hours |
| Severity triage | 7 days |
| Critical → patch release | 30 days |
| High → next minor release | per release cadence |
| Medium → next quarterly release | per release cadence |

Standard embargo: 90 days. Coordinated disclosure on reporter request.

## Severity Definitions

- **Critical:** sandbox escape, secrets exfiltration, unauthenticated remote code execution, controlled-access data egress
- **High:** authenticated remote code execution, sandbox bypass requiring local foothold, unauthorized session takeover
- **Medium:** information disclosure (non-secret), denial-of-service, sandbox refinement bypasses without escape
- **Low:** hardening recommendations, defense-in-depth improvements

## Generated-Code Execution Policy

`ecaa-workflow` executes LLM-generated code under sandbox isolation. The active sandbox profile, network policy, mounts, secret-redaction status, image digest, scan result, and exceptions are recorded per-package in `runtime/security-policy.json`.

Sandbox defaults: container isolation, default-deny egress, read-only inputs with explicit output directories, signed-image scanning, secrets isolation, per-atom `SafetyPolicy` enforcement at dispatch time.

## Senior RSE Authority

The senior research software engineer (currently Alan Huebschen) holds the authority to disable generated-code execution platform-wide during an active critical finding, without prior approval, until remediation and re-test.

## Hall of Fame

Reporters who responsibly disclose vulnerabilities will be credited here (with permission) after remediation.
