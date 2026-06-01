"""Generic bounded-parallel phase runner.

``run_phase`` schedules every work item on a ThreadPoolExecutor capped at
``max_parallel`` (the pool size IS the global concurrency cap — there is no
nested submission, so no pool deadlock). Each run is a subprocess-bound agent
call, so threads (not processes) are the right tool. A run_fn exception is
captured and returned as the item's result so one bad task can't abort the
whole sweep; the caller journals failures and continues."""
from __future__ import annotations
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Any, Callable, Optional


def run_phase(items: list, *, max_parallel: int,
              run_fn: Callable[[Any], Any],
              on_done: Optional[Callable[[Any, Any], None]] = None) -> list[tuple]:
    """Run ``run_fn(item)`` for every item, <= max_parallel at once.

    Returns a list of ``(item, result)``. If ``run_fn`` raises, the exception
    object is the result (not re-raised). ``on_done(item, result)`` fires as
    each item completes (may be called from worker threads — keep it cheap and
    thread-safe)."""
    if not items:
        return []
    workers = max(1, int(max_parallel))
    results: list[tuple] = []
    with ThreadPoolExecutor(max_workers=workers) as ex:
        fut_to_item = {ex.submit(_guard, run_fn, it): it for it in items}
        for fut in as_completed(fut_to_item):
            it = fut_to_item[fut]
            res = fut.result()
            if on_done is not None:
                on_done(it, res)
            results.append((it, res))
    return results


def _guard(run_fn, item):
    try:
        return run_fn(item)
    except Exception as e:  # captured, not raised — keeps the sweep alive
        return e
