from __future__ import annotations

import os
import subprocess
import textwrap
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]


def test_build_bio_min_local_digest_fallback_is_quiet(tmp_path: Path) -> None:
    work = tmp_path / "repo"
    (work / "scripts").mkdir(parents=True)
    (work / "containers" / "bio-min").mkdir(parents=True)
    (work / "crates" / "eval-adapters").mkdir(parents=True)
    (work / "containers" / "bio-min" / "Dockerfile").write_text("FROM scratch\n")
    (work / "scripts" / "build-bio-min.sh").write_text(
        (REPO / "scripts" / "build-bio-min.sh").read_text()
    )
    lock = work / "crates" / "eval-adapters" / "versions.lock"
    lock.write_text(
        textwrap.dedent(
            """\
            schema: "1.0"
            containers:
              bio_min:
                image: "ghcr.io/scripps/bio-min"
                digest: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            benchmarks: {}
            """
        )
    )
    subprocess.run(["git", "init", "-q"], cwd=work, check=True)

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "docker").write_text(
        textwrap.dedent(
            """\
            #!/usr/bin/env bash
            set -euo pipefail
            if [[ "$1" == "buildx" && "${2:-}" == "version" ]]; then
              exit 0
            fi
            if [[ "$1" == "buildx" && "${2:-}" == "build" ]]; then
              exit 0
            fi
            if [[ "$1" == "manifest" && "${2:-}" == "inspect" ]]; then
              exit 1
            fi
            if [[ "$1" == "image" && "${2:-}" == "inspect" ]]; then
              if [[ "$3" == --format=*RepoDigests* ]]; then
                printf 'bio-min@sha256:1111111111111111111111111111111111111111111111111111111111111111\\n'
                exit 0
              fi
              if [[ "$3" == "--format={{.Id}}" ]]; then
                printf 'sha256:2222222222222222222222222222222222222222222222222222222222222222\\n'
                exit 0
              fi
            fi
            echo "unexpected docker invocation: $*" >&2
            exit 2
            """
        )
    )
    (bin_dir / "docker").chmod(0o755)

    env = {
        **os.environ,
        "PATH": f"{bin_dir}:{os.environ['PATH']}",
        "ECAA_BUILDX_CACHE_DIR": str(tmp_path / "cache"),
    }
    result = subprocess.run(
        ["bash", "scripts/build-bio-min.sh", "bio-min:local"],
        cwd=work,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        env=env,
    )

    assert result.returncode == 0, result.stderr
    assert "Traceback" not in result.stderr
    assert "JSONDecodeError" not in result.stderr
    text = lock.read_text()
    assert 'image: "bio-min"' in text
    assert (
        'digest: "sha256:1111111111111111111111111111111111111111111111111111111111111111"'
        in text
    )
