"""Worktree attach/boot orchestrator.

Given a worktree path, make the VM running with the right mounts attached
and the SSH config updated.  Ported from:
  - scripts/tart/use-worktree-dev-vm.sh
  - scripts/tart/create-dev-vm.sh
"""

from __future__ import annotations

import os
import shlex
import time
from dataclasses import dataclass
from pathlib import Path

from remo_tart import launchd, provision, ssh, vm
from remo_tart.config import ProjectConfig
from remo_tart.console import done, get_console, step
from remo_tart.errors import RemoTartError
from remo_tart.mount import (
    MountEntry,
    manifest_read,
    manifest_write,
)
from remo_tart.paths import (
    mount_manifest_path,
    provisioned_hash_path,
    ssh_include_path,
    ssh_key_path,
    user_ssh_config_path,
    vm_log_path,
)
from remo_tart.pool import PoolConfig, resolve_pool
from remo_tart.state import Action, VmState, decide

_WAIT_INTERVAL = 2  # seconds between polls
_WAIT_ATTEMPTS = 90  # maximum polling attempts (~3 minutes total)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


_GUEST_SHARED_ROOT = "/Volumes/My Shared Files"


@dataclass(frozen=True)
class WorktreeAttachment:
    """The worktree the user just attached to the pool.

    ``host_path`` is the host directory (the git worktree top).
    ``guest_path`` is the corresponding absolute path inside the VM, computed
    as ``<shared_root>/<pool_name>/<rel-from-repo-root>``. Editors and connect
    handlers open ``guest_path``; ``host_path`` is exposed for status/logging.
    """

    pool_name: str
    host_path: Path
    guest_path: str


def guest_path_for_worktree(pool_name: str, repo_root: Path, worktree: Path) -> str:
    """Compute the guest absolute path of *worktree* under the umbrella.

    Raises :class:`RemoTartError` if *worktree* is not inside *repo_root* —
    cross-project pools are out of scope for this MVP.
    """
    repo_resolved = repo_root.resolve()
    wt_resolved = worktree.resolve()
    try:
        rel = wt_resolved.relative_to(repo_resolved)
    except ValueError as err:
        raise RemoTartError(
            f"worktree {wt_resolved} is not under repo root {repo_resolved}",
            hint="cross-repo pool membership is not yet supported",
        ) from err
    if str(rel) == ".":
        return f"{_GUEST_SHARED_ROOT}/{pool_name}"
    return f"{_GUEST_SHARED_ROOT}/{pool_name}/{rel.as_posix()}"


@dataclass(frozen=True)
class AttachOutcome:
    """Result of ``ensure_attached``.

    ``actions`` is the state-machine output for diagnostic display.
    ``manifest`` is the post-attach mount list (always one entry: the umbrella).
    ``attachment`` carries the worktree the user attached: host path + guest
    path under the umbrella. Connect handlers should read ``attachment``;
    ``manifest`` is for status reporting.
    """

    actions: tuple[Action, ...]
    manifest: tuple[MountEntry, ...]
    attachment: WorktreeAttachment


def ensure_attached(
    repo_root: Path,
    project: ProjectConfig,
    worktree_host_path: Path,
    *,
    pool_name: str | None = None,
    headless: bool = True,
) -> AttachOutcome:
    """Make the pool VM running with the umbrella mounted; idempotent.

    Always mounts ``repo_root`` once as ``<pool.name>`` — the umbrella. Adding
    a new worktree under ``repo_root`` requires no manifest change and no VM
    restart, so cross-worktree ``up`` collapses to ``NOTHING`` after the first
    boot.
    """
    pool = resolve_pool(project, pool_name)
    manifest_path = mount_manifest_path(pool.name)
    log_path = vm_log_path(pool.name)
    key_path = ssh_key_path(pool.name)

    _normalize_worktree_gitdirs(repo_root)

    umbrella_entry = MountEntry(name=pool.name, host_path=repo_root.resolve())
    manifest_write(manifest_path, [umbrella_entry])

    vm_state = _read_state(pool, expected_mount=umbrella_entry)

    mounts = manifest_read(manifest_path)
    actions = decide(vm_state)
    for action in actions:
        if action == Action.CREATE:
            _action_create(project, pool, mounts, log_path, key_path, headless=headless)
        elif action == Action.UPDATE_MOUNT_AND_RESTART:
            _action_update_mount_and_restart(project, pool, mounts, log_path, headless=headless)
        elif action == Action.START:
            _action_start(project, pool, mounts, log_path, headless=headless)
        elif action == Action.ATTACH_MOUNT_AND_START:
            _action_attach_mount_and_start(project, pool, mounts, log_path, headless=headless)
        elif action == Action.NOTHING:
            _action_nothing(pool)

    if vm.is_running(pool.name):
        _configure_ssh(project, pool, key_path)

        current_hash = provision.config_hash(project, repo_root)
        last_hash = _read_provisioned_hash(pool.name)
        config_drifted = current_hash != last_hash
        vm_changed = any(a != Action.NOTHING for a in actions)

        if vm_changed or config_drifted:
            if config_drifted and not vm_changed:
                short_old = last_hash[:8] if last_hash else "none"
                step(f"config drift detected ({short_old} → {current_hash[:8]}); reprovisioning")
            else:
                step("running packs ensure + project provision hook")
            rc = provision.run_provision(pool.name, project, mounts, verify=False)
            if rc != 0:
                raise RemoTartError(
                    f"provision failed with exit code {rc}",
                    hint="re-run with -v for verbose pack output, or check guest logs",
                )
            _write_provisioned_hash(pool.name, current_hash)

    final_manifest = manifest_read(manifest_path)
    attachment = WorktreeAttachment(
        pool_name=pool.name,
        host_path=worktree_host_path.resolve(),
        guest_path=guest_path_for_worktree(pool.name, repo_root, worktree_host_path),
    )
    return AttachOutcome(
        actions=tuple(actions),
        manifest=tuple(final_manifest),
        attachment=attachment,
    )


# ---------------------------------------------------------------------------
# Module-level helpers (patchable by tests)
# ---------------------------------------------------------------------------


def _read_provisioned_hash(vm_name: str) -> str | None:
    """Return the previously-provisioned config hash for *vm_name*, or
    None if the state file is missing or unreadable. ``None`` is
    treated as "never provisioned" by the orchestrator and forces a
    provision run.
    """
    path = provisioned_hash_path(vm_name)
    try:
        return path.read_text().strip() or None
    except (FileNotFoundError, NotADirectoryError, PermissionError, OSError):
        return None


def _write_provisioned_hash(vm_name: str, value: str) -> None:
    """Persist the most recently successfully-provisioned config hash.

    Best-effort: an IO error here doesn't fail the attach (provision
    already succeeded), it just means next ``up`` will re-provision —
    correctness over efficiency.
    """
    path = provisioned_hash_path(vm_name)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value)
    except OSError:
        pass


def _running_mount_names(vm_name: str) -> set[str]:
    """Return the directory-share names actually mounted in the running guest.

    Empty set if the guest is unreachable or the shared-files dir is empty.
    This is **ground truth** — the on-disk manifest can drift ahead of what
    the currently-running VM actually has loaded.
    """
    result = vm.exec_capture(vm_name, ["ls", "/Volumes/My Shared Files/"])
    if result.returncode != 0:
        return set()
    return {line.strip() for line in result.stdout.splitlines() if line.strip()}


def _running_mount_bindings(label_str: str) -> dict[str, Path] | None:
    """Return ``{share_name: host_path}`` for the running ``tart run`` job.

    Reads the launchd job's argv via :func:`launchd.running_tart_argv` and
    extracts each ``--dir <name>:<host_path>`` pair. Returns ``None`` when
    the job is not active so callers can distinguish "no job" from
    "job has zero bindings".
    """
    argv = launchd.running_tart_argv(label_str)
    if argv is None:
        return None
    bindings: dict[str, Path] = {}
    i = 0
    while i < len(argv):
        if argv[i] == "--dir" and i + 1 < len(argv):
            spec = argv[i + 1]
            if ":" in spec:
                name, raw_path = spec.split(":", 1)
                bindings[name] = Path(raw_path)
            i += 2
        else:
            i += 1
    return bindings


def _read_state(pool: PoolConfig, expected_mount: MountEntry | None = None) -> VmState:
    """Read current VM state for *pool*.

    ``mount_matches`` requires both:

    1. The guest can see a share named ``pool.name`` under
       ``/Volumes/My Shared Files/`` (ground truth from inside the VM).
    2. The host-side ``tart run`` argv binds that share to
       ``expected_mount.host_path`` (when provided). Comparing the host path
       catches *manifest drift* — i.e. the manifest now says the umbrella
       root but the running VM is still bound to a single worktree from a
       previous attach.

    When ``expected_mount`` is omitted, only the name-presence check runs
    (preserves prior behaviour for callers that don't have an expectation).
    """
    name = pool.name
    exists = vm.exists(name)
    running = exists and vm.is_running(name)
    if not running:
        return VmState(exists=exists, running=False, mount_matches=False)

    name_visible = pool.name in _running_mount_names(name)
    if not name_visible:
        return VmState(exists=exists, running=True, mount_matches=False)

    if expected_mount is None:
        return VmState(exists=exists, running=True, mount_matches=True)

    bindings = _running_mount_bindings(launchd.label(name))
    if bindings is None:
        return VmState(exists=exists, running=True, mount_matches=False)
    actual = bindings.get(pool.name)
    if actual is None:
        return VmState(exists=exists, running=True, mount_matches=False)
    matches = actual.resolve() == expected_mount.host_path.resolve()
    return VmState(exists=exists, running=True, mount_matches=matches)


def _normalize_worktree_gitdirs(repo_root: Path) -> None:
    """Rewrite ``<repo>/.worktrees/*/.git`` to use a *relative* gitdir.

    ``git worktree add`` writes ``gitdir: /abs/path/to/<repo>/.git/worktrees/<name>``
    which is host-specific and unreadable inside the VM. A relative path
    resolves identically on host and guest, eliminating the need for a
    guest-side symlink bridge.

    Only files whose absolute target sits under ``<repo>/.git/worktrees/`` are
    rewritten. Foreign or already-relative gitdir paths are left alone.
    """
    worktrees_dir = repo_root / ".worktrees"
    if not worktrees_dir.is_dir():
        return
    git_root = (repo_root / ".git").resolve()
    for child in worktrees_dir.iterdir():
        if not child.is_dir():
            continue
        git_file = child / ".git"
        if not git_file.is_file():
            continue
        try:
            content = git_file.read_text().strip()
        except OSError:
            continue
        if not content.startswith("gitdir:"):
            continue
        target_str = content[len("gitdir:") :].strip()
        target = Path(target_str)
        if not target.is_absolute():
            continue
        try:
            target_resolved = target.resolve()
        except OSError:
            continue
        try:
            target_resolved.relative_to(git_root)
        except ValueError:
            continue
        rel = os.path.relpath(target_resolved, child.resolve())
        git_file.write_text(f"gitdir: {rel}\n")


def _action_create(
    project: ProjectConfig,
    pool: PoolConfig,
    mounts: list[MountEntry],
    log_path: Path,
    ssh_key_path: Path,
    *,
    headless: bool = True,
) -> None:
    """Create VM from base image, configure resources, and start it."""
    name = pool.name
    label_str = launchd.label(name)

    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text("")

    launchd.remove(label_str)

    step(f"cloning base image {project.vm.base_image} as VM {name} (this can take several minutes)")
    vm.create(name, project.vm.base_image)
    step(f"configuring VM resources (cpu={project.vm.cpu}, memory={project.vm.memory_gb}G)")
    vm.set_resources(name, project.vm.cpu, project.vm.memory_gb)
    tart_args = vm.build_run_args(name, project.vm.network, mounts, headless=headless)
    step(f"submitting launchd job {label_str} ({'headless' if headless else 'with display'})")
    launchd.submit(label_str, tart_args, log_path)
    _wait_for_guest_exec(name)


def _action_update_mount_and_restart(
    project: ProjectConfig,
    pool: PoolConfig,
    mounts: list[MountEntry],
    log_path: Path,
    *,
    headless: bool = True,
) -> None:
    """Stop the VM, update mounts, and restart."""
    name = pool.name
    label_str = launchd.label(name)
    step(f"stopping VM {name} to update mounts")
    launchd.remove(label_str)
    _wait_for_stopped(name)
    for _ in range(30):
        if not launchd.job_present(label_str):
            break
        time.sleep(1)
    tart_args = vm.build_run_args(name, project.vm.network, mounts, headless=headless)
    step(f"restarting VM {name} with updated mounts ({'headless' if headless else 'with display'})")
    launchd.submit(label_str, tart_args, log_path)
    _wait_for_guest_exec(name)


def _action_start(
    project: ProjectConfig,
    pool: PoolConfig,
    mounts: list[MountEntry],
    log_path: Path,
    *,
    headless: bool = True,
) -> None:
    """Submit the VM to launchd and wait for it to be running."""
    name = pool.name
    label_str = launchd.label(name)

    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text("")

    launchd.remove(label_str)

    tart_args = vm.build_run_args(name, project.vm.network, mounts, headless=headless)
    step(f"starting VM {name} ({'headless' if headless else 'with display'})")
    launchd.submit(label_str, tart_args, log_path)
    _wait_for_guest_exec(name)


def _action_attach_mount_and_start(
    project: ProjectConfig,
    pool: PoolConfig,
    mounts: list[MountEntry],
    log_path: Path,
    *,
    headless: bool = True,
) -> None:
    """Mount is already upserted before state read; just submit and wait."""
    name = pool.name
    label_str = launchd.label(name)

    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text("")

    launchd.remove(label_str)

    tart_args = vm.build_run_args(name, project.vm.network, mounts, headless=headless)
    step(f"attaching mount and starting VM {name} ({'headless' if headless else 'with display'})")
    launchd.submit(label_str, tart_args, log_path)
    _wait_for_guest_exec(name)


def _action_nothing(pool: PoolConfig) -> None:
    """No-op — VM is already running with the correct mount."""
    get_console().print(
        f"[dim]VM '{pool.name}' is already running with the correct mount. Nothing to do.[/dim]"
    )


def _build_inject_command(pub: str) -> str:
    """Build a shell command to idempotently inject a public key into authorized_keys.

    Uses ``set -e`` + an explicit ``grep || append`` pattern so the overall
    exit code is unambiguous (0 = success).  The previous implementation
    chained ``&& ... || ... && ...`` which has surprising left-associative
    precedence and produced exit-1 even when the key was already present.
    """
    quoted_pub = shlex.quote(pub)
    return (
        "set -e; "
        "mkdir -p ~/.ssh; "
        "chmod 700 ~/.ssh; "
        "touch ~/.ssh/authorized_keys; "
        "chmod 600 ~/.ssh/authorized_keys; "
        f"grep -Fqx -- {quoted_pub} ~/.ssh/authorized_keys "
        f"|| printf '%s\\n' {quoted_pub} >> ~/.ssh/authorized_keys"
    )


def _configure_ssh(project: ProjectConfig, pool: PoolConfig, key_path: Path) -> None:
    """Configure SSH keypair, managed block, include directive, and authorized keys."""
    name = pool.name
    step(f"configuring SSH for {name}")

    # 1. Generate keypair (idempotent).
    ssh.generate_keypair(key_path)

    # 2. Build and upsert the managed SSH config block.
    block = ssh.managed_block(name, project.vm.guest_user, key_path)
    include_path = ssh_include_path()
    ssh.upsert_managed_block(include_path, name, block)

    # 3. Ensure the include directive is in the user's SSH config.
    user_config = user_ssh_config_path()
    ssh.ensure_include_in_user_config(user_config, include_path)

    # 4. Inject the public key into the guest's authorized_keys (idempotent).
    pub = ssh.public_key(key_path)
    inject_cmd = _build_inject_command(pub)
    result = vm.exec_capture(name, ["sh", "-c", inject_cmd])
    if result.returncode != 0:
        stderr = (result.stderr or "").strip()
        stdout = (result.stdout or "").strip()
        detail = stderr or stdout or "(no output)"
        raise RemoTartError(
            f"failed to install ssh public key into guest (exit {result.returncode}): {detail}",
            hint=f"check {vm_log_path(name)} and ensure the VM is fully booted",
        )
    done(f"SSH ready: ssh tart-{name}")


# ---------------------------------------------------------------------------
# Wait helpers
# ---------------------------------------------------------------------------


def _wait_for_guest_exec(vm_name: str, *, attempts: int = 300, interval: float = 2.0) -> None:
    """Poll `tart exec true` until success. Raises RemoTartError on timeout.

    "Running" at the Tart/VZ layer just means the VM kernel started; macOS
    inside still needs time to boot its launchd, network, and exec agent. We
    surface intermediate signals (IP assignment, last log line) so the wait
    isn't opaque.
    """
    step(f"waiting for {vm_name} guest agent (macOS boot inside the VM)...")
    get_console().print(
        "[dim]   typical wait: 3-5 minutes (10+ on first boot). "
        "Don't Ctrl-C — the connect step runs only after this finishes.[/dim]"
    )
    log_path = vm_log_path(vm_name)
    last_ip: str | None = None
    last_log_tail: str = ""
    for i in range(attempts):
        if vm.is_running(vm_name):
            result = vm.exec_capture(vm_name, ["/usr/bin/true"])
            if result.returncode == 0:
                done(f"VM {vm_name} guest agent is ready")
                return

        elapsed = int((i + 1) * interval)
        # IP assignment lands ~10-20s before the exec agent on macOS.
        if last_ip is None:
            ip = vm.ip_address(vm_name)
            if ip:
                last_ip = ip
                get_console().print(f"[dim]  ...network up: {ip}[/dim]")

        if elapsed and elapsed % 10 == 0:
            running = "running" if vm.is_running(vm_name) else "VZ-still-booting"
            tail = _last_log_line(log_path)
            extra = ""
            if tail and tail != last_log_tail:
                last_log_tail = tail
                extra = f' last log: "{tail[:80]}"'
            get_console().print(f"[dim]  ...{vm_name} {running} ({elapsed}s elapsed){extra}[/dim]")
        time.sleep(interval)
    raise RemoTartError(
        f"timeout waiting for VM '{vm_name}' to be ready for exec",
        hint=f"check {log_path}",
    )


def _last_log_line(log_path: Path) -> str:
    """Return the last non-empty line of a log file, or empty string."""
    try:
        with log_path.open("rb") as f:
            f.seek(0, 2)
            size = f.tell()
            if size == 0:
                return ""
            chunk = min(2048, size)
            f.seek(size - chunk, 0)
            data = f.read(chunk).decode("utf-8", errors="replace")
        for line in reversed(data.splitlines()):
            if line.strip():
                return line.strip()
    except OSError:
        pass
    return ""


def _wait_for_stopped(name: str) -> None:
    """Poll until the VM has stopped, or raise RemoTartError on timeout."""
    for i in range(_WAIT_ATTEMPTS):
        if not vm.is_running(name):
            return
        elapsed = int((i + 1) * _WAIT_INTERVAL)
        if elapsed and elapsed % 10 == 0:
            get_console().print(f"[dim]  ...waiting for {name} to stop ({elapsed}s)[/dim]")
        time.sleep(_WAIT_INTERVAL)
    raise RemoTartError(
        f"timeout waiting for VM '{name}' to stop",
        hint=f"check {vm_log_path(name)}",
    )
