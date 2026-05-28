import { mkdtempSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test, expect } from '@playwright/test'
import {
  PASILLA_TESTDATA,
  wirePasillaRuntimeInputs,
} from '../helpers/pasillaFixture'

test('wirePasillaRuntimeInputs registers local GMT and SME decisions', async () => {
  const pkg = mkdtempSync(join(tmpdir(), 'pasilla-fixture-'))

  await wirePasillaRuntimeInputs(pkg)

  const inputs = JSON.parse(
    readFileSync(join(pkg, 'runtime', 'inputs.json'), 'utf8'),
  ) as Array<{ label: string; root_path: string; files: Array<{ relative_path: string }> }>
  const gmtInput = inputs.find((input) => input.label.includes('gene sets'))
  expect(gmtInput?.root_path).toContain('runtime/inputs/pathway_enrichment')
  expect(gmtInput?.files.map((f) => f.relative_path)).toContain('drosophila_pasilla_mini.gmt')

  const pathwayDecisions = JSON.parse(
    readFileSync(
      join(pkg, 'runtime', 'outputs', 'pathway_enrichment', 'sme-decisions.json'),
      'utf8',
    ),
  ) as { decisions: Array<{ id: string; chosen: string }> }
  expect(pathwayDecisions.decisions).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ id: 'gene_set_source', chosen: 'upload_gmt' }),
      expect.objectContaining({ id: 'method_substitution', chosen: 'use_gseapy_prerank' }),
    ]),
  )

  const rawQcDecisions = JSON.parse(
    readFileSync(
      join(pkg, 'runtime', 'outputs', 'raw_qc', 'sme-decisions.json'),
      'utf8',
    ),
  ) as { decisions: Array<{ id: string; chosen: string }> }
  expect(rawQcDecisions.decisions).toContainEqual(
    expect.objectContaining({
      id: 'raw_qc_disposition',
      chosen: 'drop_stage_from_workflow',
    }),
  )

  expect(PASILLA_TESTDATA).toContain('testdata/scenarios/atoms/bulk-rnaseq-pasilla')
})
