// React.lazy wrappers for the 11 inspector tabs. Each named export in
// The source files is wrapped via the.then() default-shape adapter so
// the source files keep their named exports (and the existing tests
// keep importing them by name).
//
// StateInspectorPane consumes these instead of the eager named imports,
// and wraps the active tab in a <Suspense> with a placeholder fallback.

import { lazy } from 'react'

export const LazyPlanTab = lazy(() =>
  import('./PlanTab').then((m) => ({ default: m.PlanTab })),
)
export const LazyStateTab = lazy(() =>
  import('./StateTab').then((m) => ({ default: m.StateTab })),
)
export const LazyDocumentsPane = lazy(() =>
  import('./DocumentsTab').then((m) => ({ default: m.DocumentsPane })),
)
export const LazyJobsFeed = lazy(() =>
  import('./JobsTab').then((m) => ({ default: m.JobsFeed })),
)
export const LazyMetricsTable = lazy(() =>
  import('./MetricsTab').then((m) => ({ default: m.MetricsTable })),
)
export const LazyHistoryPane = lazy(() =>
  import('./HistoryTab').then((m) => ({ default: m.HistoryPane })),
)
export const LazyFiguresPane = lazy(() =>
  import('./FiguresTab').then((m) => ({ default: m.FiguresPane })),
)
export const LazyDashboardPane = lazy(() =>
  import('./DashboardTab').then((m) => ({ default: m.DashboardPane })),
)
export const LazyDecisionsTab = lazy(() =>
  import('./DecisionsTab').then((m) => ({ default: m.DecisionsTab })),
)
export const LazyCompareTab = lazy(() =>
  import('./CompareTab').then((m) => ({ default: m.CompareTab })),
)
export const LazyInputsTab = lazy(() =>
  import('./InputsTab').then((m) => ({ default: m.InputsTab })),
)
export const LazyCompositionTab = lazy(() =>
  import('./CompositionTab').then((m) => ({ default: m.CompositionTab })),
)
// V3+v4 residuals closure pending repair-strategy proposals.
export const LazyRepairsTab = lazy(() =>
  import('./RepairsTab').then((m) => ({ default: m.RepairsTab })),
)
