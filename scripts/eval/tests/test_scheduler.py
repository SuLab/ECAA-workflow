import threading
import time
from scripts.eval.scheduler import run_phase


def test_runs_every_item_and_collects_results():
    items = [1, 2, 3, 4, 5]
    out = run_phase(items, max_parallel=2, run_fn=lambda x: x * 10)
    assert sorted(r for _, r in out) == [10, 20, 30, 40, 50]
    assert {it for it, _ in out} == set(items)


def test_respects_concurrency_cap():
    peak = {"n": 0, "cur": 0}
    lock = threading.Lock()

    def run_fn(_):
        with lock:
            peak["cur"] += 1
            peak["n"] = max(peak["n"], peak["cur"])
        time.sleep(0.02)
        with lock:
            peak["cur"] -= 1
        return 1

    run_phase(list(range(20)), max_parallel=3, run_fn=run_fn)
    assert peak["n"] <= 3
    assert peak["n"] >= 2  # actually parallelized


def test_on_done_called_per_item():
    seen = []
    lock = threading.Lock()

    def on_done(item, result):
        with lock:
            seen.append((item, result))

    run_phase([1, 2, 3], max_parallel=2, run_fn=lambda x: x + 1, on_done=on_done)
    assert sorted(seen) == [(1, 2), (2, 3), (3, 4)]


def test_run_fn_exception_is_captured_not_raised():
    def run_fn(x):
        if x == 2:
            raise RuntimeError("boom")
        return x

    out = dict(run_phase([1, 2, 3], max_parallel=2, run_fn=run_fn))
    assert out[1] == 1 and out[3] == 3
    assert isinstance(out[2], Exception)
    assert "boom" in str(out[2])
