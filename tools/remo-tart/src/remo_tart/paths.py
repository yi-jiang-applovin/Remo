"""Centralised on-disk paths and repo-root discovery."""

from __future__ import annotations

from pathlib import Path

from remo_tart.errors import RemoTartError


def _config_root() -> Path:
    return Path.home() / ".config" / "remo" / "tart"


def state_dir(vm_name: str) -> Path:
    del vm_name  # reserved; state dir is shared across VMs today
    return _config_root()


def mount_manifest_path(vm_name: str) -> Path:
    return _config_root() / f"{vm_name}.mounts"


def vm_log_path(vm_name: str) -> Path:
    return _config_root() / f"{vm_name}.log"


def provisioned_hash_path(vm_name: str) -> Path:
    """State file recording the config hash that was last successfully
    provisioned into this VM. Compared on every ``up`` to detect drift
    between what's installed in the VM and what the on-disk config now
    asks for; mismatch forces a re-run of ``provision.run_provision``
    even when the VM-state machine would otherwise return ``NOTHING``.

    File contents: a single line with the hex SHA-256 (no trailing
    newline). Missing file = "never provisioned" (also forces
    provision).
    """
    return _config_root() / f"{vm_name}.provisioned-hash"


def ssh_include_path() -> Path:
    return _config_root() / "ssh_config"


def ssh_key_path(vm_name: str) -> Path:
    return _config_root() / "ssh" / f"{vm_name}_ed25519"


def user_ssh_config_path() -> Path:
    return Path.home() / ".ssh" / "config"


def find_repo_root(start: Path | None = None) -> Path:
    """Resolve the Tart-managed project root containing ``.tart/project.toml``.

    Strategy:

    1. If *start* (or cwd) is inside a git repo, ask git for ``--git-common-dir``
       and use its parent as the candidate. ``--git-common-dir`` returns the
       *main* repo's ``.git`` even when invoked from a linked worktree, so this
       step skips past any per-worktree stray ``.tart/`` shadows. If the
       resolved candidate has ``.tart/project.toml``, return it.
    2. Otherwise (or as a fallback for non-git directories), walk upward from
       *start* and return the first ancestor with ``.tart/project.toml``.

    Raises :class:`RemoTartError` if no candidate yields a project file.
    """
    import subprocess

    current = (start or Path.cwd()).resolve()

    try:
        result = subprocess.run(
            [
                "git",
                "-C",
                str(current),
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        common_dir = Path(result.stdout.strip()).resolve()
        # `--git-common-dir` points at the main repo's .git directory; its
        # parent is the main checkout root. For linked worktrees this differs
        # from `--show-toplevel`, which is exactly what we want here.
        candidate = common_dir.parent
        if (candidate / ".tart" / "project.toml").is_file():
            return candidate
    except (subprocess.CalledProcessError, FileNotFoundError):
        pass

    for candidate in [current, *current.parents]:
        if (candidate / ".tart" / "project.toml").is_file():
            return candidate
    raise RemoTartError(
        "unable to find the Remo repo root from the current working directory",
        hint="run remo-tart from inside a Tart-managed project (.tart/project.toml must exist)",
    )


def git_worktree_root(path: Path) -> Path:
    """Return the worktree root containing *path*, or *path* if not in a repo.

    Uses ``git rev-parse --show-toplevel``. For the main checkout this is the
    repo root; for a linked worktree (e.g. under ``.worktrees/<name>``) it is
    the worktree's own top-level directory.
    """
    import subprocess

    try:
        result = subprocess.run(
            ["git", "-C", str(path), "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            check=True,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return path.resolve()
    return Path(result.stdout.strip()).resolve()
