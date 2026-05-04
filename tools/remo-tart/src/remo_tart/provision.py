"""Guest-side provisioning orchestrator.

Builds a bash script that runs inside the tart VM to source each enabled
pack, call its ``tart_pack_<name>_ensure`` function, run the project-level
provision hook, and optionally run the verify-worktree hook.

Also exposes :func:`config_hash`, which deterministically hashes every
input that affects what provision installs (enabled packs, pack contents,
``_lib.sh``, ``provision.sh``). Used by the orchestrator to detect
config drift between attaches and force a reprovision when the on-disk
config diverges from what was last provisioned into the VM.
"""

from __future__ import annotations

import hashlib
import shlex
from pathlib import Path

from remo_tart import vm
from remo_tart.config import ProjectConfig
from remo_tart.errors import RemoTartError
from remo_tart.mount import MountEntry

_SHARED_ROOT = "/Volumes/My Shared Files"


def _always_quote(s: str) -> str:
    """Always wrap *s* in double-quotes, escaping ``"`` and ``$`` inside."""
    escaped = s.replace("\\", "\\\\").replace('"', '\\"').replace("$", "\\$").replace("`", "\\`")
    return f'"{escaped}"'


def _primary_mount(mounts: list[MountEntry]) -> MountEntry:
    """Return the first mount that is not the git-root bridge.

    Raises :exc:`~remo_tart.errors.RemoTartError` if no such mount exists.
    """
    for entry in mounts:
        if not entry.name.endswith("-git-root"):
            return entry
    raise RemoTartError(
        "no primary mount found — all mounts are git-root bridges",
        hint="add at least one worktree mount before calling provision",
    )


def build_guest_script(
    project: ProjectConfig,
    mounts: list[MountEntry],
    packs_dir_guest: str,
    *,
    verify: bool,
) -> str:
    """Return a bash script the guest will run to provision itself."""
    primary = _primary_mount(mounts)
    primary_guest_path = f"{_SHARED_ROOT}/{primary.name}"

    lines: list[str] = [
        "#!/usr/bin/env bash",
        "set -euo pipefail",
        "",
    ]

    # Source the shared helper library first; packs depend on its functions
    # (retry_command, ensure_xcode, ensure_node_and_npm, ensure_codex,
    # remo_tart_worktree_env_exports).
    lines.append(f"source {_always_quote(f'{packs_dir_guest}/_lib.sh')}")
    lines.append("")

    # Source each enabled pack
    for pack in project.packs:
        pack_path = f"{packs_dir_guest}/{pack}.sh"
        lines.append(f"source {_always_quote(pack_path)}")

    if project.packs:
        lines.append("")

    # Call the ensure function for each enabled pack. Pass the worktree root
    # (the primary mount's guest path) so pack functions can locate the
    # in-tree .tart/<pack-cache>/ directories. Packs that don't need it
    # simply ignore extra args.
    for pack in project.packs:
        lines.append(f"tart_pack_{pack}_ensure {shlex.quote(primary_guest_path)}")

    if project.packs:
        lines.append("")

    # Run project-level provision hook
    provision_path = f"{primary_guest_path}/.tart/provision.sh"
    lines.append(f"bash {shlex.quote(provision_path)}")

    # Optionally run verify-worktree
    if verify:
        lines.append("")
        verify_path = f"{primary_guest_path}/.tart/verify-worktree.sh"
        lines.append(f"bash {shlex.quote(verify_path)}")

    lines.append("")
    return "\n".join(lines)


def run_provision(
    vm_name: str,
    project: ProjectConfig,
    mounts: list[MountEntry],
    *,
    verify: bool = True,
) -> int:
    """Ship the guest script via vm.exec_interactive, return exit code."""
    packs_dir = _packs_dir_guest(mounts)
    script = build_guest_script(project, mounts, packs_dir_guest=packs_dir, verify=verify)
    return vm.exec_interactive(vm_name, ["bash", "-c", script])


def _packs_dir_guest(mounts: list[MountEntry]) -> str:
    """Derive the guest-side packs directory from the git-root bridge mount.

    The packs dir lives under the git-root bridge at ``.tart/packs``.
    Falls back to the primary mount if no git-root bridge exists.
    """
    for entry in mounts:
        if entry.name.endswith("-git-root"):
            return f"{_SHARED_ROOT}/{entry.name}/.tart/packs"
    primary = _primary_mount(mounts)
    return f"{_SHARED_ROOT}/{primary.name}/.tart/packs"


def config_hash(project: ProjectConfig, repo_root: Path) -> str:
    """Hex SHA-256 of every input that affects what provision installs.

    Inputs (mixed in deterministic order):

    * ``project.packs`` enabled list (sorted) — adding/removing a pack
      changes the hash even if no file content changed.
    * ``.tart/packs/_lib.sh`` content — shared helpers; one byte changed
      here can affect every pack's behaviour.
    * Each enabled pack's ``.tart/packs/<name>.sh`` content.
    * ``project.scripts.provision`` content (resolved against
      *repo_root*) — picks up ``provision.sh`` edits like adding a
      ``claude plugin install`` line.

    Deliberately *not* hashed:

    * VM resources (``cpu``, ``memory_gb``, ``base_image``,
      ``network``) — these don't influence what's installed; changing
      them needs a VM restart, not a re-provision. ``doctor`` is the
      right place to surface those discrepancies.
    * ``project.scripts.verify_worktree`` — verify is a smoke test, not
      a state-mutating step; rerunning provision over verify changes
      doesn't help.
    * Files outside ``packs/`` and the provision script — provision can
      only see what packs install.

    Returns the hex digest. Missing input files contribute the literal
    string ``"<missing>"`` so renaming a pack file is detected even if
    the rename happens to leave content identical.
    """
    h = hashlib.sha256()

    enabled = sorted(project.packs)
    h.update(b"enabled\0")
    for name in enabled:
        h.update(name.encode("utf-8"))
        h.update(b"\0")
    h.update(b"\1")

    packs_dir = repo_root / ".tart" / "packs"
    h.update(b"_lib.sh\0")
    h.update(_read_for_hash(packs_dir / "_lib.sh"))
    h.update(b"\1")

    for name in enabled:
        h.update(f"pack:{name}\0".encode())
        h.update(_read_for_hash(packs_dir / f"{name}.sh"))
        h.update(b"\1")

    h.update(b"provision\0")
    h.update(_read_for_hash(repo_root / project.scripts.provision))
    h.update(b"\1")

    return h.hexdigest()


def _read_for_hash(path: Path) -> bytes:
    """Read *path*'s bytes, or return a sentinel marker if it doesn't
    exist. Missing files contribute *something* to the hash so that
    deleting a referenced file flips the digest.
    """
    try:
        return path.read_bytes()
    except (FileNotFoundError, IsADirectoryError, PermissionError):
        return b"<missing>"
