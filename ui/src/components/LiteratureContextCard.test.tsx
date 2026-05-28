import { describe, expect, test } from "vitest";
import { render, screen } from "@testing-library/react";
import axe from "axe-core";
import { LiteratureContextCard } from "./LiteratureContextCard";
import type { LiteratureContext } from "../types/LiteratureContext";

const greenCtx: LiteratureContext = {
  entity: "ACAN",
  entity_kind: "gene",
  prior_rows: [
    {
      entity: "ACAN",
      entity_kind: "gene",
      pmid: "28123456",
      evidence_quote: "ACAN reduction in disc",
      source_kind: "pmc_oa_full_text",
      source_hash: "sha256:abc",
      redistributable: true,
    },
  ],
  finding_rows: [
    {
      finding_id: "ENSG00000157766",
      entity: "ACAN",
      entity_kind: "gene",
      prior_pmids: ["28123456"],
      concordance_flag: "same_direction",
      evidence_quote: "ACAN reduction in disc",
      source_kind: "pmc_oa_full_text",
    },
  ],
  source_artifacts: [],
  source_scope: "pmc_oa",
};

describe("LiteratureContextCard", () => {
  test("renders entity and PMID-anchored row", () => {
    render(<LiteratureContextCard ctx={greenCtx} />);
    expect(screen.getByText("ACAN")).toBeInTheDocument();
    expect(screen.getByText("28123456")).toBeInTheDocument();
    expect(screen.getByText(/same direction/i)).toBeInTheDocument();
  });

  test("renders empty state when both arrays empty", () => {
    const empty: LiteratureContext = {
      ...greenCtx,
      prior_rows: [],
      finding_rows: [],
    };
    render(<LiteratureContextCard ctx={empty} />);
    expect(screen.getByText(/No prior literature/i)).toBeInTheDocument();
  });

  test("flags non-redistributable rows with a lock indicator", () => {
    const paywalled: LiteratureContext = {
      ...greenCtx,
      prior_rows: [
        {
          ...greenCtx.prior_rows[0]!,
          source_kind: "external_pdf_local_only",
          redistributable: false,
        },
      ],
    };
    render(<LiteratureContextCard ctx={paywalled} />);
    expect(screen.getByLabelText(/not redistributable/i)).toBeInTheDocument();
  });

  test("has no axe a11y violations on the green path", async () => {
    const { container } = render(<LiteratureContextCard ctx={greenCtx} />);
    const results = await axe.run(container, {
      runOnly: {
        type: "tag",
        values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"],
      },
      rules: {
        "color-contrast": { enabled: false },
      },
    });
    expect(results.violations).toEqual([]);
  });
});
