import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { createHash } from 'node:crypto'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, type Page } from '@playwright/test'

const __dirname = dirname(fileURLToPath(import.meta.url))
export const REPO_ROOT = resolve(__dirname, '..', '..')
export const PASILLA_TESTDATA = join(
  REPO_ROOT,
  'testdata',
  'scenarios',
  'atoms',
  'bulk-rnaseq-pasilla',
)

const PASILLA_COUNTS = 'counts.tsv'
const PASILLA_SAMPLES = 'samples.csv'
const PASILLA_GMT = 'drosophila_pasilla_mini.gmt'
const FIXED_REGISTERED_AT = '2026-05-25T00:00:00.000Z'

interface RuntimeInputFile {
  relative_path: string
  relpath: string
  size_bytes: number
  sha256: string
  mime_type: string
}

function fileEntry(root: string, filename: string, mimeType: string): RuntimeInputFile {
  const path = join(root, filename)
  const bytes = readFileSync(path)
  return {
    relative_path: filename,
    relpath: filename,
    size_bytes: statSync(path).size,
    sha256: createHash('sha256').update(bytes).digest('hex'),
    mime_type: mimeType,
  }
}

function writeSmeDecisions(
  packagePath: string,
  taskId: string,
  decisions: Array<{ id: string; chosen: string; rationale: string }>,
): void {
  const outDir = join(packagePath, 'runtime', 'outputs', taskId)
  mkdirSync(outDir, { recursive: true })
  writeFileSync(
    join(outDir, 'sme-decisions.json'),
    JSON.stringify(
      {
        task_id: taskId,
        timestamp: FIXED_REGISTERED_AT,
        decisions,
      },
      null,
      2,
    ),
  )
}

/**
 * Register the staged pasilla directory through the real Inputs tab. This
 * makes the session's pre-emit DAG see that the SME supplied a counts matrix,
 * while `wirePasillaRuntimeInputs` later rewrites runtime/inputs.json to
 * package-local paths that the execution container can read.
 */
export async function registerPasillaInputViaUi(
  page: Page,
  screenshotPath?: string,
): Promise<void> {
  await page.locator('#state-tab-inputs').click()
  await expect(page.getByTestId('inputs-tab')).toBeVisible({ timeout: 10_000 })
  await page.getByTestId('inputs-path-field').fill(PASILLA_TESTDATA)
  await page.getByTestId('inputs-label-field').fill('pasilla Drosophila counts + annotation')
  await page.getByTestId('inputs-register-button').click()
  await expect(page.getByTestId('inputs-tab')).toContainText(
    'pasilla Drosophila counts + annotation',
    { timeout: 60_000 },
  )
  if (screenshotPath) {
    await page.screenshot({ path: screenshotPath, fullPage: true, timeout: 30_000 })
  }
}

/**
 * Wire small local runtime inputs for the live pasilla execution:
 * counts/annotation for data_acquisition, a deterministic mini GMT for
 * pathway enrichment, and SME decisions for local-only substitutions.
 */
export async function wirePasillaRuntimeInputs(packagePath: string): Promise<void> {
  const runtimeDir = join(packagePath, 'runtime')
  const pasillaInputsDir = join(runtimeDir, 'inputs', 'pasilla_data')
  const pathwayInputsDir = join(runtimeDir, 'inputs', 'pathway_enrichment')
  mkdirSync(pasillaInputsDir, { recursive: true })
  mkdirSync(pathwayInputsDir, { recursive: true })

  const countsSource = join(PASILLA_TESTDATA, PASILLA_COUNTS)
  const samplesSource = join(PASILLA_TESTDATA, PASILLA_SAMPLES)
  const gmtSource = join(PASILLA_TESTDATA, PASILLA_GMT)

  for (const path of [countsSource, samplesSource, gmtSource]) {
    if (!existsSync(path)) throw new Error(`pasilla fixture not found at ${path}`)
  }

  copyFileSync(countsSource, join(pasillaInputsDir, 'pasilla_gene_counts.tsv'))
  copyFileSync(samplesSource, join(pasillaInputsDir, 'pasilla_sample_annotation.csv'))
  copyFileSync(gmtSource, join(pathwayInputsDir, PASILLA_GMT))

  const inputs = [
    {
      input_id: 'pasilla_data_01',
      label: 'pasilla Drosophila counts + annotation',
      kind: 'local_path',
      root_path: pasillaInputsDir,
      files: [
        fileEntry(pasillaInputsDir, 'pasilla_gene_counts.tsv', 'text/tab-separated-values'),
        fileEntry(pasillaInputsDir, 'pasilla_sample_annotation.csv', 'text/csv'),
      ],
      registered_at: FIXED_REGISTERED_AT,
      registered_by: 'e2e-test',
    },
    {
      input_id: 'pasilla_gene_sets_01',
      label: 'pasilla Drosophila mini gene sets',
      kind: 'local_path',
      root_path: pathwayInputsDir,
      files: [fileEntry(pathwayInputsDir, PASILLA_GMT, 'text/plain')],
      registered_at: FIXED_REGISTERED_AT,
      registered_by: 'e2e-test',
    },
  ]

  writeFileSync(join(runtimeDir, 'inputs.json'), JSON.stringify(inputs, null, 2))

  writeSmeDecisions(packagePath, 'pathway_enrichment', [
    {
      id: 'gene_set_source',
      chosen: 'upload_gmt',
      rationale: 'Use the package-local Drosophila mini GMT registered under runtime/inputs/pathway_enrichment.',
    },
    {
      id: 'method_substitution',
      chosen: 'use_gseapy_prerank',
      rationale:
        'bio-min:local ships R 4.5.2 + Rscript, but BiocManager::install("fgsea") triggers a 10+ minute Rcpp / RcppParallel C++ compile that defeats the smoke-test tractability budget. gseapy installs in ~30 seconds via `pip install --user gseapy` and runs preranked GSEA on the DESeq2 Wald statistics in seconds. Neither tool is preinstalled in bio-min:local; choose gseapy for the fast-install path.',
    },
  ])
  writeSmeDecisions(packagePath, 'raw_qc', [
    {
      id: 'raw_qc_disposition',
      chosen: 'drop_stage_from_workflow',
      rationale: 'The pasilla fixture supplies a prequantified counts matrix; no FASTQ/BAM read-level inputs are staged.',
    },
  ])
}
