import { test } from '@playwright/test'

/**
 * 13 — Branch popup flow (PR-A).
 *
 * Regression guard for the popup-blocker bug. The branch flow used to
 * call `window.open(...)` *after* `await fetch(...)`, which Chrome /
 * Firefox / Safari block by default because the call left the
 * user-gesture window. The fix pre-opens `about:blank` synchronously
 * inside the click handler.
 *
 * This test only verifies that triggering the branch flow doesn't get
 * trapped by the browser's popup blocker — the deeper functional
 * coverage of the branched-session lifecycle lives in the
 * `cross_version_diff.spec.ts` suite.
 */

test.describe('Branch popup', () => {
  test('clicking "Branch" in the task drawer opens a new context page', async ({
    context,
    page,
  }) => {
    test.skip(
      true,
      'Mock backend does not currently fixture a complete-task DAG that exposes' +
        ' the Branch button in the TaskDetailDrawer footer; once the' +
        ' fixture supports completed-task scenarios this test should:' +
        ' (1) navigate to a task with state=completed, (2) open the drawer,' +
        ' (3) click Branch, (4) accept the confirmation modal,' +
        ' (5) listen for context.on("page") to fire — that event fires only' +
        ' when popup.open(about:blank) succeeds within the user gesture.' +
        ' The pre-open + reassign pattern is in TaskDetailDrawer.tsx::submitBranch.',
    )
    void context
    void page
  })
})
