"""Guest-side provisioning orchestrator.

Builds a bash script that runs inside the tart VM to materialize host-read
pack sources into a guest temp directory, source each enabled pack, call its
``tart_pack_<name>_ensure`` function, and run the project-level provision
hook.

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


def _require_file(path: Path, description: str) -> str:
    if not path.is_file():
        raise RemoTartError(
            f"{description} is missing: {path}",
            hint="check .tart/project.toml packs.enabled",
        )
    return path.read_text()


def _quote_guest_tmp_path(path: str) -> str:
    """Quote a guest path while preserving the runtime-expanded $tmpdir."""
    if path.startswith("$tmpdir/"):
        suffix = path.removeprefix("$tmpdir/")
        escaped = suffix.replace("\\", "\\\\").replace('"', '\\"').replace("$", "\\$")
        return f'"$tmpdir/{escaped}"'
    return shlex.quote(path)


def _heredoc(path: str, content: str, label: str) -> list[str]:
    digest = hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
    delimiter = f"__REMO_TART_{label}_{digest}__"
    if delimiter in content:
        delimiter = f"{delimiter}_END"
    return [
        f"cat > {_quote_guest_tmp_path(path)} <<'{delimiter}'",
        content,
        delimiter,
    ]


def build_guest_script(project: ProjectConfig, repo_root: Path) -> str:
    """Return a bash script the guest will run to provision itself."""
    packs_dir = repo_root / ".tart" / "packs"
    lib_source = _require_file(packs_dir / "_lib.sh", "pack library")
    pack_sources = [
        (
            pack,
            _require_file(packs_dir / f"{pack}.sh", "pack file"),
        )
        for pack in project.packs
    ]
    provision_source = _require_file(repo_root / project.scripts.provision, "provision script")

    lines: list[str] = [
        "#!/usr/bin/env bash",
        "set -euo pipefail",
        'tmpdir="$(mktemp -d)"',
        "trap 'rm -rf \"$tmpdir\"' EXIT",
        'mkdir -p "$HOME/Developer"',
        "",
    ]

    lines.extend(_heredoc("$tmpdir/_lib.sh", lib_source, "LIB"))
    lines.append("")

    for pack, source in pack_sources:
        lines.extend(_heredoc(f"$tmpdir/{pack}.sh", source, f"PACK_{pack.upper()}"))
        lines.append("")

    lines.extend(_heredoc("$tmpdir/provision.sh", provision_source, "PROVISION"))
    lines.append("")

    lines.append('source "$tmpdir/_lib.sh"')
    for pack, _source in pack_sources:
        lines.append(f'source "$tmpdir/{pack}.sh"')

    if project.packs:
        lines.append("")

    for pack, _source in pack_sources:
        lines.append(f'tart_pack_{pack}_ensure "$HOME/Developer"')

    if project.packs:
        lines.append("")

    lines.append('bash "$tmpdir/provision.sh"')
    lines.append("")
    return "\n".join(lines)


def run_provision(
    vm_name: str,
    project: ProjectConfig,
    repo_root: Path,
    *,
    verify: bool = False,
) -> int:
    """Ship the guest script via vm.exec_script, return exit code."""
    _ = verify
    script = build_guest_script(project, repo_root)
    return vm.exec_script(vm_name, script)


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
