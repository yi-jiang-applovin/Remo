from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from remo_tart.config import ProjectConfig, ScriptsConfig, VmConfig
from remo_tart.errors import RemoTartError
from remo_tart.provision import build_guest_script, run_provision


def _cfg(packs: list[str] | None = None) -> ProjectConfig:
    return ProjectConfig(
        slug="remo",
        vm=VmConfig(
            name="remo-dev",
            base_image="img",
            cpu=1,
            memory_gb=1,
            network="shared",
        ),
        packs=packs if packs is not None else ["ios", "rust"],
        scripts=ScriptsConfig(
            provision=".tart/provision.sh",
            verify_worktree=".tart/verify-worktree.sh",
        ),
    )


def _seed_repo(root: Path, *, pack_files: dict[str, str], provision_body: str) -> Path:
    """Seed a fake repo at *root* with .tart/packs/ and provision.sh.

    Returns *root* for chaining. Each entry of *pack_files* maps pack
    basename (without .sh) to its content; `_lib` becomes `_lib.sh`.
    Pass an empty *pack_files* to test missing-file handling.
    """
    packs = root / ".tart" / "packs"
    packs.mkdir(parents=True, exist_ok=True)
    for name, body in pack_files.items():
        (packs / f"{name}.sh").write_text(body)
    (root / ".tart" / "provision.sh").write_text(provision_body)
    return root


def test_script_has_shebang_and_strict_mode(tmp_path: Path) -> None:
    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "ios", "rust": "rust"},
        provision_body=":",
    )
    script = build_guest_script(_cfg(), tmp_path)
    assert script.startswith("#!/usr/bin/env bash")
    assert "set -euo pipefail" in script


def test_build_guest_script_embeds_pack_sources_and_creates_developer(tmp_path: Path) -> None:
    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib body", "ios": "ios body"},
        provision_body="echo provision",
    )
    script = build_guest_script(_cfg(["ios"]), tmp_path)

    assert 'mkdir -p "$HOME/Developer"' in script
    assert "lib body" in script
    assert "ios body" in script
    assert "echo provision" in script
    assert "/Volumes/My Shared Files" not in script


def test_script_sources_lib_before_any_pack(tmp_path: Path) -> None:
    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "ios", "rust": "rust"},
        provision_body=":",
    )
    script = build_guest_script(_cfg(["ios", "rust"]), tmp_path)
    lib_idx = script.index("_lib.sh")
    ios_idx = script.index("ios.sh")
    rust_idx = script.index("rust.sh")
    assert lib_idx < ios_idx
    assert lib_idx < rust_idx


def test_script_calls_ensure_function_for_each_pack(tmp_path: Path) -> None:
    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "ios", "rust": "rust"},
        provision_body=":",
    )
    script = build_guest_script(_cfg(["ios", "rust"]), tmp_path)
    assert "tart_pack_ios_ensure" in script
    assert "tart_pack_rust_ensure" in script


def test_script_passes_developer_dir_to_each_ensure(tmp_path: Path) -> None:
    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "shell": "shell", "ios": "ios", "rust": "rust"},
        provision_body=":",
    )
    script = build_guest_script(_cfg(["shell", "ios", "rust"]), tmp_path)
    assert 'tart_pack_shell_ensure "$HOME/Developer"' in script
    assert 'tart_pack_ios_ensure "$HOME/Developer"' in script
    assert 'tart_pack_rust_ensure "$HOME/Developer"' in script


def test_script_runs_embedded_project_provision_hook(tmp_path: Path) -> None:
    _seed_repo(tmp_path, pack_files={"_lib": "lib", "ios": "ios"}, provision_body="provision")
    script = build_guest_script(_cfg(["ios"]), tmp_path)
    assert 'bash "$tmpdir/provision.sh"' in script
    assert "provision" in script


def test_build_guest_script_fails_when_enabled_pack_file_missing(tmp_path: Path) -> None:
    _seed_repo(tmp_path, pack_files={"_lib": "lib"}, provision_body="echo provision")

    with pytest.raises(RemoTartError) as excinfo:
        build_guest_script(_cfg(["ios"]), tmp_path)

    assert "pack file is missing" in str(excinfo.value)


def test_script_with_empty_packs_list(tmp_path: Path) -> None:
    _seed_repo(tmp_path, pack_files={"_lib": "lib"}, provision_body=":")
    script = build_guest_script(_cfg(packs=[]), tmp_path)
    assert 'source "$tmpdir/_lib.sh"' in script
    assert "tart_pack_" not in script
    assert "provision.sh" in script


@patch("remo_tart.provision.vm.exec_script")
def test_run_provision_streams_script(exec_script: MagicMock, tmp_path: Path) -> None:
    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "tart_pack_ios_ensure() { :; }"},
        provision_body=":",
    )
    exec_script.return_value = 0

    assert run_provision("remo-dev", _cfg(["ios"]), tmp_path) == 0
    assert exec_script.call_args.args[0] == "remo-dev"
    assert 'mkdir -p "$HOME/Developer"' in exec_script.call_args.args[1]


@patch("remo_tart.provision.vm.exec_script")
def test_run_provision_propagates_nonzero(exec_script: MagicMock, tmp_path: Path) -> None:
    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "tart_pack_ios_ensure() { :; }"},
        provision_body=":",
    )
    exec_script.return_value = 7
    assert run_provision("remo-dev", _cfg(["ios"]), tmp_path, verify=False) == 7


# ---------------------------------------------------------------------------
# config_hash (rebuild detection — issue #68 phase 1)
# ---------------------------------------------------------------------------


def test_config_hash_is_deterministic(tmp_path: Path) -> None:
    from remo_tart.provision import config_hash

    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "ios body", "node": "node body"},
        provision_body="bootstrap",
    )
    cfg = _cfg(["ios", "node"])
    h1 = config_hash(cfg, tmp_path)
    h2 = config_hash(cfg, tmp_path)
    assert h1 == h2
    assert isinstance(h1, str)
    assert len(h1) == 64  # sha256 hex


def test_config_hash_changes_when_pack_added_to_enabled(tmp_path: Path) -> None:
    """Adding a pack to enabled flips the hash even if no file content
    changed (the new pack file already existed but wasn't being run)."""
    from remo_tart.provision import config_hash

    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "ios", "node": "node", "uv": "uv body"},
        provision_body="bootstrap",
    )
    h_without_uv = config_hash(_cfg(["ios", "node"]), tmp_path)
    h_with_uv = config_hash(_cfg(["ios", "node", "uv"]), tmp_path)
    assert h_without_uv != h_with_uv


def test_config_hash_changes_when_pack_content_changes(tmp_path: Path) -> None:
    from remo_tart.provision import config_hash

    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "ios v1"},
        provision_body="bootstrap",
    )
    h_v1 = config_hash(_cfg(["ios"]), tmp_path)
    (tmp_path / ".tart" / "packs" / "ios.sh").write_text("ios v2 — added mint check")
    h_v2 = config_hash(_cfg(["ios"]), tmp_path)
    assert h_v1 != h_v2


def test_config_hash_changes_when_provision_changes(tmp_path: Path) -> None:
    from remo_tart.provision import config_hash

    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "ios"},
        provision_body="mint bootstrap",
    )
    h1 = config_hash(_cfg(["ios"]), tmp_path)
    (tmp_path / ".tart" / "provision.sh").write_text(
        "mint bootstrap\nclaude plugin install figma@claude-plugins-official"
    )
    h2 = config_hash(_cfg(["ios"]), tmp_path)
    assert h1 != h2


def test_config_hash_changes_when_lib_changes(tmp_path: Path) -> None:
    """`_lib.sh` is shared by all packs; any change there can affect
    every pack's behaviour, so it must be in the hash."""
    from remo_tart.provision import config_hash

    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib v1", "ios": "ios"},
        provision_body="bootstrap",
    )
    h1 = config_hash(_cfg(["ios"]), tmp_path)
    (tmp_path / ".tart" / "packs" / "_lib.sh").write_text("lib v2 — new helper")
    h2 = config_hash(_cfg(["ios"]), tmp_path)
    assert h1 != h2


def test_config_hash_ignores_vm_resource_changes(tmp_path: Path) -> None:
    """Changing vm.cpu / memory / base_image shouldn't trigger reprovision —
    those affect VM boot, not what's installed. Reprovisioning won't fix
    them anyway. Doctor is the right place for that drift signal."""
    from remo_tart.provision import config_hash

    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "ios": "ios"},
        provision_body="bootstrap",
    )
    cfg_small = _cfg(["ios"])
    cfg_big = ProjectConfig(
        slug="remo",
        vm=VmConfig(
            name="remo-dev",
            base_image="img",
            cpu=99,  # ← changed
            memory_gb=64,  # ← changed
            network="shared",
        ),
        packs=["ios"],
        scripts=ScriptsConfig(
            provision=".tart/provision.sh",
            verify_worktree=".tart/verify-worktree.sh",
        ),
    )
    assert config_hash(cfg_small, tmp_path) == config_hash(cfg_big, tmp_path)


def test_config_hash_handles_missing_pack_file(tmp_path: Path) -> None:
    """If a pack is enabled but its file doesn't exist, hash still
    computes (so `up` doesn't crash with FileNotFoundError) but is
    distinct from the same setup with the file present — so resolving
    the missing file flips the hash."""
    from remo_tart.provision import config_hash

    (tmp_path / ".tart" / "packs").mkdir(parents=True)
    (tmp_path / ".tart" / "packs" / "_lib.sh").write_text("lib")
    (tmp_path / ".tart" / "provision.sh").write_text("bootstrap")
    # Note: ios.sh deliberately not created.

    h_missing = config_hash(_cfg(["ios"]), tmp_path)

    (tmp_path / ".tart" / "packs" / "ios.sh").write_text("real content")
    h_present = config_hash(_cfg(["ios"]), tmp_path)
    assert h_missing != h_present


def test_config_hash_pack_order_in_enabled_doesnt_matter_when_normalized(
    tmp_path: Path,
) -> None:
    """``packs.enabled`` is sorted before hashing so reordering it in
    project.toml doesn't trigger a spurious reprovision."""
    from remo_tart.provision import config_hash

    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib", "a": "a body", "b": "b body"},
        provision_body="bootstrap",
    )
    h_ab = config_hash(_cfg(["a", "b"]), tmp_path)
    h_ba = config_hash(_cfg(["b", "a"]), tmp_path)
    assert h_ab == h_ba
