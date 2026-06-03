"""The committed evidence dir must escape the docs/ gitignore jail and exist."""
from pathlib import Path
import subprocess

REPO_ROOT = Path(__file__).resolve().parents[3]


def test_eval_results_dir_is_tracked():
    d = REPO_ROOT / "docs" / "eval-results"
    assert (d / ".gitkeep").exists()
    assert (d / "README.md").exists()


def test_eval_results_not_ignored_by_git():
    # git check-ignore exits 0 when a path IS ignored; we want non-zero (NOT ignored).
    target = "docs/eval-results/.gitkeep"
    r = subprocess.run(["git", "check-ignore", target],
                       cwd=str(REPO_ROOT), capture_output=True, text=True)
    assert r.returncode != 0, (
        f"{target} is gitignored (check-ignore matched: {r.stdout!r}); "
        "the !docs/eval-results/ negation is missing or out of order")


def test_gitignore_has_negation():
    gi = (REPO_ROOT / ".gitignore").read_text()
    assert "!docs/eval-results/" in gi
