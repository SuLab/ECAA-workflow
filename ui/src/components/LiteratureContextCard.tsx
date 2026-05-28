import React from "react";
import type { LiteratureContext } from "../types/LiteratureContext";
import type { ConcordanceFlag } from "../types/ConcordanceFlag";
import type { SourceKind } from "../types/SourceKind";

function flagBackground(f: ConcordanceFlag): string {
  switch (f) {
    case "same_direction":
      return "#1f7a3a";
    case "opposite_direction":
      return "#a8202e";
    case "unverifiable":
      return "#a8741f";
    case "no_prior_finding":
      return "#666";
  }
}

function sourceKindLabel(k: SourceKind): string {
  switch (k) {
    case "pmc_oa_full_text":
      return "PMC OA full text";
    case "abstract_only":
      return "Abstract only";
    case "external_pdf_local_only":
      return "Local-only PDF";
    case "none":
      return "—";
  }
}

export interface LiteratureContextCardProps {
  ctx: LiteratureContext;
}

export const LiteratureContextCard: React.FC<LiteratureContextCardProps> = ({
  ctx,
}) => {
  const hasContent =
    ctx.prior_rows.length + ctx.finding_rows.length > 0;

  return (
    <div
      role="region"
      aria-label={`Literature context for ${ctx.entity}`}
      style={{
        border: "1px solid #ccc",
        borderRadius: 6,
        padding: 12,
        fontFamily: "system-ui",
        background: "#fff",
        maxWidth: 720,
      }}
    >
      <header
        style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8 }}
      >
        <strong style={{ fontSize: 16 }}>{ctx.entity}</strong>
        <span
          style={{
            fontSize: 11,
            padding: "2px 6px",
            border: "1px solid #999",
            borderRadius: 4,
            textTransform: "lowercase",
          }}
        >
          {ctx.entity_kind}
        </span>
        <span
          style={{
            fontSize: 11,
            color: "#666",
            marginLeft: "auto",
          }}
        >
          source scope: {ctx.source_scope.replace(/_/g, " ")}
        </span>
      </header>

      {!hasContent && (
        <p style={{ color: "#666", fontStyle: "italic", margin: "8px 0" }}>
          No prior literature in scope for this entity.
        </p>
      )}

      {ctx.prior_rows.length > 0 && (
        <section aria-labelledby="lit-prior-h" style={{ marginBottom: 12 }}>
          <h3
            id="lit-prior-h"
            style={{ fontSize: 13, margin: "8px 0 4px" }}
          >
            Prior findings ({ctx.prior_rows.length})
          </h3>
          <table
            style={{ width: "100%", fontSize: 12, borderCollapse: "collapse" }}
          >
            <thead>
              <tr style={{ background: "#f3f3f3" }}>
                <th style={{ textAlign: "left", padding: 4, width: 80 }}>
                  PMID
                </th>
                <th style={{ textAlign: "left", padding: 4, width: 110 }}>
                  Source
                </th>
                <th style={{ textAlign: "left", padding: 4 }}>Evidence</th>
              </tr>
            </thead>
            <tbody>
              {ctx.prior_rows.map((r, i) => (
                <tr key={`prior-${i}`}>
                  <td style={{ padding: 4 }}>
                    <a
                      href={`https://pubmed.ncbi.nlm.nih.gov/${r.pmid}/`}
                      target="_blank"
                      rel="noopener noreferrer"
                    >
                      {r.pmid}
                    </a>
                  </td>
                  <td style={{ padding: 4 }}>
                    <span
                      style={{
                        fontSize: 10,
                        padding: "1px 4px",
                        border: "1px solid #ccc",
                        borderRadius: 3,
                      }}
                    >
                      {sourceKindLabel(r.source_kind)}
                    </span>
                    {!r.redistributable && (
                      <span
                        title="not redistributable — local-only"
                        aria-label="not redistributable"
                        style={{ marginLeft: 4 }}
                      >
                        🔒
                      </span>
                    )}
                  </td>
                  <td
                    style={{
                      padding: 4,
                      fontFamily:
                        "ui-monospace, SFMono-Regular, monospace",
                      wordBreak: "break-word",
                    }}
                  >
                    {r.evidence_quote}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}

      {ctx.finding_rows.length > 0 && (
        <section aria-labelledby="lit-findings-h">
          <h3
            id="lit-findings-h"
            style={{ fontSize: 13, margin: "8px 0 4px" }}
          >
            Analysis alignment ({ctx.finding_rows.length})
          </h3>
          <table
            style={{ width: "100%", fontSize: 12, borderCollapse: "collapse" }}
          >
            <thead>
              <tr style={{ background: "#f3f3f3" }}>
                <th style={{ textAlign: "left", padding: 4, width: 140 }}>
                  Finding
                </th>
                <th style={{ textAlign: "left", padding: 4, width: 140 }}>
                  Concordance
                </th>
                <th style={{ textAlign: "left", padding: 4 }}>Evidence</th>
              </tr>
            </thead>
            <tbody>
              {ctx.finding_rows.map((r, i) => (
                <tr key={`finding-${i}`}>
                  <td
                    style={{
                      padding: 4,
                      fontFamily:
                        "ui-monospace, SFMono-Regular, monospace",
                      wordBreak: "break-word",
                    }}
                  >
                    {r.finding_id}
                  </td>
                  <td style={{ padding: 4 }}>
                    <span
                      style={{
                        fontSize: 11,
                        color: "white",
                        background: flagBackground(r.concordance_flag),
                        padding: "1px 6px",
                        borderRadius: 3,
                      }}
                    >
                      {r.concordance_flag.replace(/_/g, " ")}
                    </span>
                  </td>
                  <td
                    style={{
                      padding: 4,
                      fontFamily:
                        "ui-monospace, SFMono-Regular, monospace",
                      wordBreak: "break-word",
                    }}
                  >
                    {r.evidence_quote || "—"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}
    </div>
  );
};
