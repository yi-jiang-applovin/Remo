# Remo Tart VM-Owned Workspace Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Change `remo-tart` so it creates a ready macOS VM with an empty VM-local `~/Developer` directory instead of using the host worktree as the active workspace.

**Architecture:** Keep the existing `remo_tart` Python CLI, launchd, SSH, and pack concepts, but remove host source mounts from the normal path. Provisioning scripts are read from the host checkout and streamed into the VM; editor commands open `/Users/<guest_user>/Developer`.

**Tech Stack:** Python 3.11, Click, Pydantic, pytest, Ruff, Tart CLI, macOS SSH, VS Code/Cursor Remote SSH.

---

## Context

Approved spec: `docs/superpowers/specs/2026-05-28-remo-tart-vm-owned-workspace-design.md`

Relevant skills for execution:

- @superpowers:test-driven-development for each behavior change.
- @superpowers:verification-before-completion before claiming completion.

Subagent note: this harness exposes subagent tools, but use them only if the active harness/user permissions allow it. Otherwise execute the plan locally with @superpowers:executing-plans.

## File Structure

Modify these files:

- `tools/remo-tart/src/remo_tart/connect.py`
  - Own VM-local editor path helpers and Remote SSH URI construction.
- `tools/remo-tart/src/remo_tart/provision.py`
  - Read pack/provision files from host.
  - Build guest script without mount paths.
  - Stream script into VM instead of `bash -c <script>`.
- `tools/remo-tart/src/remo_tart/vm.py`
  - Add `exec_script(vm_name, script)` helper that invokes `tart exec -i <vm> bash -s` with stdin.
- `tools/remo-tart/src/remo_tart/worktree.py`
  - Convert default lifecycle from "attach worktree" to "ensure VM ready".
  - Keep or rename carefully to reduce churn, but remove host worktree mount semantics from public behavior.
- `tools/remo-tart/src/remo_tart/cli.py`
  - Route `up`, `use`, `connect`, and `bootstrap` through VM-local workspace behavior.
  - Make `use PATH` fail clearly.
  - Start VMs without source mounts.
- `tools/remo-tart/src/remo_tart/status.py`
  - Report VM-local workspace path.
  - Treat mount manifest as legacy diagnostic state only.
- `tools/remo-tart/src/remo_tart/doctor.py`
  - Stop treating missing mount manifest and missing git-root bridge as issues.
  - Keep pack-file checks.
- `.tart/provision.sh`
  - Make this a no-op VM bootstrap hook; remove `make setup`.
- `.tart/packs/_lib.sh`
  - Stop emitting worktree-local `.tart` cache exports.
  - Add missing Rust helper functions or move them into `rust.sh`.
- `.tart/packs/shell.sh`
  - Create/write VM-local environment for `~/Developer`.
- `.tart/packs/ios.sh`, `.tart/packs/rust.sh`, `.tart/packs/node.sh`, `.tart/packs/python.sh`, `.tart/packs/go.sh`
  - Remove generated-state exports that point under a worktree `.tart`.
- `README.md`
- `docs/tart-development-guide.md`
- `docs/tart-dev-vm.md`
- `skills/tart-dev-management/SKILL.md`
  - Document the new "clone inside VM" workflow.

Modify tests:

- `tools/remo-tart/tests/test_connect.py`
- `tools/remo-tart/tests/test_provision.py`
- `tools/remo-tart/tests/test_worktree.py`
- `tools/remo-tart/tests/test_cli.py`
- `tools/remo-tart/tests/test_status.py`
- `tools/remo-tart/tests/test_doctor.py`
- Add focused pack-script tests if no existing test cleanly covers `.tart/packs/*.sh` behavior.

---

## Chunk 1: VM-Local Editor Path

### Task 1: Make connection helpers open `/Users/<guest_user>/Developer`

**Files:**
- Modify: `tools/remo-tart/src/remo_tart/connect.py`
- Modify tests: `tools/remo-tart/tests/test_connect.py`

- [ ] **Step 1: Write failing tests for the VM-local developer path**

In `tools/remo-tart/tests/test_connect.py`, remove dependency on `WorktreeAttachment` and add:

```python
def test_developer_dir_for_guest_user() -> None:
    from remo_tart.connect import developer_dir

    assert developer_dir("admin") == "/Users/admin/Developer"
```

Update the VS Code test to call `connect_vscode("remo-dev", "admin")` and expect:

```python
"vscode-remote://ssh-remote+tart-remo-dev/Users/admin/Developer"
```

Update the Cursor test to call `connect_cursor("remo-dev", "admin")` and expect:

```python
"vscode-remote://ssh-remote+tart-remo-dev/Users/admin/Developer"
```

- [ ] **Step 2: Run the focused failing tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_connect.py -q
```

Expected: failures because `developer_dir` does not exist and editor helpers still require `WorktreeAttachment`.

- [ ] **Step 3: Implement `developer_dir` and simplify editor helpers**

In `connect.py`, add:

```python
def developer_dir(guest_user: str) -> str:
    return f"/Users/{guest_user}/Developer"
```

Change signatures:

```python
def connect_vscode(
    vm_name: str,
    guest_user: str,
    *,
    new_window: bool = False,
) -> int:
    alias = ssh_alias(vm_name)
    uri = f"vscode-remote://ssh-remote+{alias}{developer_dir(guest_user)}"
    window_flag = "--new-window" if new_window else "--reuse-window"
    argv = ["code", window_flag, "--folder-uri", uri]
    return subprocess.run(argv, check=False).returncode
```

Do the same for `connect_cursor`.

Remove the import of `WorktreeAttachment` from `connect.py`.

- [ ] **Step 4: Run focused tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_connect.py -q
```

Expected: `test_connect.py` passes.

- [ ] **Step 5: Commit chunk 1**

Run:

```bash
git add tools/remo-tart/src/remo_tart/connect.py tools/remo-tart/tests/test_connect.py
git commit -m "refactor: open vm-local developer workspace"
```

---

## Chunk 2: Stream Provisioning Into The VM

### Task 2: Add a VM script execution helper

**Files:**
- Modify: `tools/remo-tart/src/remo_tart/vm.py`
- Modify tests: `tools/remo-tart/tests/test_vm.py`

- [ ] **Step 1: Write failing test for script stdin execution**

In `tools/remo-tart/tests/test_vm.py`, add:

```python
@patch("remo_tart.vm.subprocess.run")
def test_exec_script_streams_script_to_bash_stdin(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0)

    assert vm.exec_script("remo-dev", "echo hi\n") == 0

    run.assert_called_once()
    assert run.call_args.args[0] == ["tart", "exec", "-i", "remo-dev", "bash", "-s"]
    assert run.call_args.kwargs["input"] == "echo hi\n"
    assert run.call_args.kwargs["text"] is True
    assert run.call_args.kwargs["check"] is False
```

- [ ] **Step 2: Run failing test**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_vm.py::test_exec_script_streams_script_to_bash_stdin -q
```

Expected: failure because `exec_script` does not exist.

- [ ] **Step 3: Implement `exec_script`**

In `vm.py`, add:

```python
def exec_script(name: str, script: str) -> int:
    result = subprocess.run(
        ["tart", "exec", "-i", name, "bash", "-s"],
        input=script,
        text=True,
        check=False,
    )
    return result.returncode
```

Add a short docstring noting that Tart requires `-i` to forward host stdin to
the guest command.

- [ ] **Step 4: Run focused test**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_vm.py::test_exec_script_streams_script_to_bash_stdin -q
```

Expected: pass.

### Task 3: Build guest provisioning script from host file contents

**Files:**
- Modify: `tools/remo-tart/src/remo_tart/provision.py`
- Modify tests: `tools/remo-tart/tests/test_provision.py`

- [ ] **Step 1: Replace mount-based tests with source-shipping tests**

In `tests/test_provision.py`, adjust helpers so `_seed_repo()` writes `_lib.sh`, enabled pack files, and `.tart/provision.sh`.

Add or update tests for:

```python
def test_build_guest_script_embeds_pack_sources_and_creates_developer(tmp_path: Path) -> None:
    _seed_repo(
        tmp_path,
        pack_files={"_lib": "lib body", "ios": "ios body"},
        provision_body="echo provision",
    )
    script = build_guest_script(_cfg(["ios"]), tmp_path)

    assert "mkdir -p \"$HOME/Developer\"" in script
    assert "lib body" in script
    assert "ios body" in script
    assert "echo provision" in script
    assert "/Volumes/My Shared Files" not in script
```

Add:

```python
def test_build_guest_script_fails_when_enabled_pack_file_missing(tmp_path: Path) -> None:
    _seed_repo(tmp_path, pack_files={"_lib": "lib"}, provision_body="echo provision")

    with pytest.raises(RemoTartError) as excinfo:
        build_guest_script(_cfg(["ios"]), tmp_path)

    assert "pack file is missing" in str(excinfo.value)
```

Add:

```python
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
    assert "mkdir -p \"$HOME/Developer\"" in exec_script.call_args.args[1]
```

- [ ] **Step 2: Run failing provision tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_provision.py -q
```

Expected: failures because current provisioning requires mounts and uses `vm.exec_interactive`.

- [ ] **Step 3: Implement host-source provisioning**

In `provision.py`:

1. Remove `_primary_mount()` and `_packs_dir_guest()` from normal provisioning.
2. Add helpers:

```python
def _require_file(path: Path, description: str) -> str:
    if not path.is_file():
        raise RemoTartError(f"{description} is missing: {path}", hint="check .tart/project.toml packs.enabled")
    return path.read_text()
```

3. Add a heredoc helper that avoids delimiter collisions:

```python
def _heredoc(path: str, content: str, label: str) -> list[str]:
    digest = hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
    delimiter = f"__REMO_TART_{label}_{digest}__"
    if delimiter in content:
        delimiter = f"{delimiter}_END"
    return [
        f"cat > {shlex.quote(path)} <<'{delimiter}'",
        content,
        delimiter,
    ]
```

4. Change `build_guest_script(project, repo_root)` to:

```python
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
mkdir -p "$HOME/Developer"
```

Then write `_lib.sh`, each pack script, and `provision.sh` into `$tmpdir`, source them from `$tmpdir`, call `tart_pack_<name>_ensure "$HOME/Developer"`, and run `bash "$tmpdir/provision.sh"`.

5. Change `run_provision(vm_name, project, repo_root, *, verify=False)` to call `vm.exec_script(vm_name, script)`.

Keep `verify` accepted only for compatibility, but do not run `verify_worktree`.

- [ ] **Step 4: Run focused provision tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_provision.py -q
```

Expected: pass.

- [ ] **Step 5: Commit chunk 2**

Run:

```bash
git add tools/remo-tart/src/remo_tart/vm.py tools/remo-tart/src/remo_tart/provision.py tools/remo-tart/tests/test_vm.py tools/remo-tart/tests/test_provision.py
git commit -m "refactor: stream tart provisioning into vm"
```

---

## Chunk 3: Replace Worktree Attach With VM Readiness

### Task 4: Introduce VM-ready lifecycle result

**Files:**
- Modify: `tools/remo-tart/src/remo_tart/worktree.py`
- Modify tests: `tools/remo-tart/tests/test_worktree.py`

- [ ] **Step 1: Write failing tests for no host mount manifest**

In `tests/test_worktree.py`, replace umbrella-mount expectations with VM-ready expectations:

```python
def test_ensure_ready_does_not_write_mount_manifest(fake_home: Path, fake_repo: Path) -> None:
    from remo_tart.paths import mount_manifest_path
    from remo_tart.worktree import ensure_ready

    with (
        patch("remo_tart.worktree._read_state") as read,
        patch("remo_tart.worktree._action_nothing"),
        patch("remo_tart.worktree._configure_ssh"),
        patch("remo_tart.worktree.vm.is_running", return_value=True),
        patch("remo_tart.worktree.provision.run_provision", return_value=0),
    ):
        read.return_value = VmState(exists=True, running=True, mount_matches=True)
        outcome = ensure_ready(fake_repo, _cfg())

    assert not mount_manifest_path("remo-dev").exists()
    assert outcome.pool_name == "remo-dev"
```

Add:

```python
def test_ensure_ready_passes_repo_root_to_provision(fake_home: Path, fake_repo: Path) -> None:
    from remo_tart.worktree import ensure_ready

    with (
        patch("remo_tart.worktree._read_state") as read,
        patch("remo_tart.worktree._action_nothing"),
        patch("remo_tart.worktree._configure_ssh"),
        patch("remo_tart.worktree.vm.is_running", return_value=True),
        patch("remo_tart.worktree.provision.run_provision", return_value=0) as run_provision,
    ):
        read.return_value = VmState(exists=True, running=True, mount_matches=True)
        ensure_ready(fake_repo, _cfg())

    assert run_provision.call_args.args[2] == fake_repo
```

Remove or rewrite tests for `guest_path_for_worktree`, `_normalize_worktree_gitdirs`, `_running_mount_bindings`, and mount-drift detection; those behaviors should no longer be part of normal lifecycle.

- [ ] **Step 2: Run failing worktree tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_worktree.py -q
```

Expected: failures because current lifecycle still writes mount manifests and returns `AttachOutcome`.

- [ ] **Step 3: Implement `ReadyOutcome` and `ensure_ready`**

In `worktree.py`:

1. Replace `WorktreeAttachment` and `AttachOutcome` with:

```python
@dataclass(frozen=True)
class ReadyOutcome:
    actions: tuple[Action, ...]
    pool_name: str
```

2. Add:

```python
def ensure_ready(
    repo_root: Path,
    project: ProjectConfig,
    *,
    pool_name: str | None = None,
    headless: bool = True,
) -> ReadyOutcome:
    pool = resolve_pool(project, pool_name)
    state = _read_state(pool)
    actions = decide(state)
    # Run the selected VM action, configure SSH, and provision from repo_root.
```

3. Resolve pool, read state without expected mount, and run actions.
4. Do not call `manifest_write`.
5. Do not call `_normalize_worktree_gitdirs`.
6. Pass `[]` mounts to `_action_create`, `_action_start`, and `_action_attach_mount_and_start`.
7. Call `provision.run_provision(pool.name, project, repo_root, verify=False)` when VM changed or config hash drifted.
8. Keep `_configure_ssh`, `_wait_for_guest_exec`, `_build_inject_command`, and hash handling.

For compatibility during the refactor, either delete `ensure_attached` and update callers in the same chunk, or leave:

```python
def ensure_attached(*args: object, **kwargs: object) -> ReadyOutcome:
    raise RemoTartError("host worktree attach has been removed", hint="clone source inside the VM under ~/Developer")
```

Only keep this wrapper if tests need a clear old-API failure.

- [ ] **Step 4: Simplify VM state reading**

Change `_read_state(pool)` so it checks only:

- `vm.exists(pool.name)`
- `vm.is_running(pool.name)` if it exists

Return:

```python
VmState(exists=False, running=False, mount_matches=False)
VmState(exists=True, running=False, mount_matches=True)
VmState(exists=True, running=True, mount_matches=True)
```

This preserves the existing `decide()` state machine without mount probing. `UPDATE_MOUNT_AND_RESTART` should not occur in the normal path.

- [ ] **Step 5: Run focused worktree tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_worktree.py -q
```

Expected: pass after obsolete mount/worktree tests are removed or rewritten.

### Task 5: Update CLI routing for VM-ready lifecycle

**Files:**
- Modify: `tools/remo-tart/src/remo_tart/cli.py`
- Modify tests: `tools/remo-tart/tests/test_cli.py`

- [ ] **Step 1: Write failing CLI tests**

In `tests/test_cli.py`:

1. Replace `_attach_outcome()` with `_ready_outcome()`:

```python
def _ready_outcome(pool_name: str = "remo-dev", actions: tuple = (Action.NOTHING,)) -> object:
    from remo_tart.worktree import ReadyOutcome

    return ReadyOutcome(actions=actions, pool_name=pool_name)
```

2. Update `up` tests to patch `remo_tart.cli.worktree.ensure_ready`.
3. Assert `up vscode` calls `connect_vscode("remo-dev", "admin")` with no attachment.
4. Assert `connect vscode` calls `connect_vscode("remo-dev", "admin")`.
5. Replace `test_use_with_explicit_path` with:

```python
def test_use_with_explicit_path_errors(fake_repo: Path) -> None:
    runner = CliRunner()
    result = runner.invoke(main, ["use", str(fake_repo)])
    assert result.exit_code == 1
    assert "host worktree attach has been removed" in result.output
```

6. Update `start` test to assert `vm.build_run_args` receives an empty mount list.

- [ ] **Step 2: Run failing CLI tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_cli.py -q
```

Expected: failures because CLI still uses `ensure_attached`, `git_worktree_root`, attachments, and manifests.

- [ ] **Step 3: Implement CLI changes**

In `cli.py`:

1. In `up`, remove `git_worktree_root(Path.cwd())`.
2. Call:

```python
outcome = worktree.ensure_ready(repo, project, pool_name=pool_name, headless=not display)
```

3. Use `outcome.pool_name` for connect calls.
4. For editor connect calls:

```python
_connect.connect_vscode(name, project.vm.guest_user)
_connect.connect_cursor(name, project.vm.guest_user)
```

5. In `use`, reject `worktree_path`:

```python
if worktree_path:
    raise RemoTartError(
        "host worktree attach has been removed",
        hint="run `remo-tart up` and clone source inside the VM under ~/Developer",
    )
```

6. In `use`, call `ensure_ready`.
7. In `connect`, remove `WorktreeAttachment` construction and call editor helpers with VM/user only.
8. In `start`, use `mounts = []` instead of `manifest_read`.
9. In `bootstrap`, call `ensure_ready`.

Leave `clean-worktree` as a legacy cleanup command for now unless tests/docs make it confusing. Do not reference it in new daily workflow docs.

- [ ] **Step 4: Run focused CLI tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_cli.py -q
```

Expected: pass.

- [ ] **Step 5: Commit chunk 3**

Run:

```bash
git add tools/remo-tart/src/remo_tart/worktree.py tools/remo-tart/src/remo_tart/cli.py tools/remo-tart/tests/test_worktree.py tools/remo-tart/tests/test_cli.py
git commit -m "refactor: make remo-tart ensure vm readiness"
```

---

## Chunk 4: Status, Doctor, And Pack Environment

### Task 6: Update status and doctor for no default mounts

**Files:**
- Modify: `tools/remo-tart/src/remo_tart/status.py`
- Modify: `tools/remo-tart/src/remo_tart/doctor.py`
- Modify tests: `tools/remo-tart/tests/test_status.py`
- Modify tests: `tools/remo-tart/tests/test_doctor.py`

- [ ] **Step 1: Write failing status tests**

In `tests/test_status.py`, update collect calls to include `guest_user="admin"` or derive workspace path from the project in CLI tests.

Add assertions:

```python
assert data["workspace"]["developer_path"] == "/Users/admin/Developer"
```

Update `render_human` expected sections to include `workspace:`.

Keep `mounts` in status as legacy manifest state, but do not require selected mount to match the current worktree.

- [ ] **Step 2: Write failing doctor tests**

In `tests/test_doctor.py`, update or add tests that prove:

```python
findings = run_all("remo-dev", repo_root)
assert not any(f.level == "issue" and "git-root bridge" in f.message for f in findings)
assert not any(f.level == "issue" and "mount manifest" in f.message for f in findings)
```

Missing pack files should still be issues.

- [ ] **Step 3: Run failing tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_status.py tests/test_doctor.py -q
```

Expected: failures due current mount-centric status/doctor behavior.

- [ ] **Step 4: Implement status changes**

In `status.py`:

1. Change signature to:

```python
def collect(vm_name: str, repo_root: Path, guest_user: str) -> dict:
```

2. Add:

```python
"workspace": {
    "developer_path": f"/Users/{guest_user}/Developer",
}
```

3. Keep `mounts` section, but label it as legacy in human output:

```text
legacy_mounts:
```

or keep `mounts:` if changing output is too much churn. In either case, do not imply selected active workspace.

4. Update `cli.status` to pass `project.vm.guest_user`.

- [ ] **Step 5: Implement doctor changes**

In `doctor.py`:

1. Replace `_check_manifest` with a legacy diagnostic that returns warnings only:

```python
if not manifest_path.exists():
    return [Finding("ok", "source mount manifest not required in vm-owned workspace")], []
```

2. Remove `_check_git_root_bridge` from the required check path.
3. Keep `_check_packs`, `_check_ssh_key`, and `_check_ssh_include`.

- [ ] **Step 6: Run focused tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_status.py tests/test_doctor.py -q
```

Expected: pass.

### Task 7: Make packs VM-local

**Files:**
- Modify: `.tart/provision.sh`
- Modify: `.tart/packs/_lib.sh`
- Modify: `.tart/packs/shell.sh`
- Modify: `.tart/packs/ios.sh`
- Modify: `.tart/packs/rust.sh`
- Modify: `.tart/packs/node.sh`
- Modify: `.tart/packs/python.sh`
- Modify: `.tart/packs/go.sh`
- Add or modify tests if a pack-script test file exists; otherwise cover through `test_provision.py` script output and `rg` verification.

- [ ] **Step 1: Write static verification command into the task checklist**

Before editing, run:

```bash
rg -n "DerivedData|CARGO_TARGET_DIR|npm_config_cache|PIP_CACHE_DIR|GOCACHE|GOMODCACHE|\\.tart/" .tart/packs .tart/provision.sh
```

Expected: current matches show worktree-local generated-state exports.

- [ ] **Step 2: Update `.tart/provision.sh`**

Replace `make setup` with an idempotent no-op bootstrap hook:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Project-specific VM bootstrap hook.
# Source checkouts are created manually inside ~/Developer, so there is no
# repository-specific setup to run during VM creation.
:
```

- [ ] **Step 3: Update shell environment**

In `.tart/packs/shell.sh`, make `_shell_write_worktree_env` write only VM-local values:

```bash
{
    printf '# Generated by .tart/packs/shell.sh - do not edit by hand.\n'
    printf 'export REMO_TART_DEVELOPER_DIR="$HOME/Developer"\n'
    printf 'export PATH="$HOME/.cargo/bin:$PATH"\n'
} > "${env_file}"
```

Rename the function to `_shell_write_developer_env` if convenient, but do not over-refactor.

- [ ] **Step 4: Remove shared-worktree generated-state exports**

In these files, remove or neutralize `tart_pack_*_worktree_env_exports` functions:

- `.tart/packs/ios.sh`
- `.tart/packs/rust.sh`
- `.tart/packs/node.sh`
- `.tart/packs/python.sh`
- `.tart/packs/go.sh`

Do not export:

- `REMO_TART_DERIVED_DATA`
- `CARGO_TARGET_DIR`
- `npm_config_cache`
- `VIRTUAL_ENV`
- `PIP_CACHE_DIR`
- `GOCACHE`
- `GOMODCACHE`

Default tool locations inside the VM are acceptable.

- [ ] **Step 5: Add missing Rust helper implementations**

The current Rust pack calls `ensure_rustup`, `ensure_rust_targets`, and `ensure_cbindgen`, but `_lib.sh` does not define them. Add these helpers to `.tart/packs/_lib.sh` or inline them into `.tart/packs/rust.sh`.

Use this behavior:

```bash
ensure_rustup() {
    if command -v rustup >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
        return 0
    fi
    retry_command "install rustup" /bin/sh -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'
}

ensure_rust_targets() {
    rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
}

ensure_cbindgen() {
    if command -v cbindgen >/dev/null 2>&1; then
        return 0
    fi
    retry_command "install cbindgen" cargo install cbindgen
}
```

Keep `export PATH="${HOME}/.cargo/bin:${PATH}"` before target/cbindgen setup.

- [ ] **Step 6: Run static verification**

Run:

```bash
rg -n "DerivedData|CARGO_TARGET_DIR|npm_config_cache|PIP_CACHE_DIR|GOCACHE|GOMODCACHE|\\.tart/" .tart/packs .tart/provision.sh
```

Expected: no generated-state exports under `.tart`; references in comments are acceptable only if they describe removed legacy behavior.

- [ ] **Step 7: Run focused tests**

Run:

```bash
cd tools/remo-tart
uv run pytest tests/test_provision.py -q
```

Expected: pass.

- [ ] **Step 8: Commit chunk 4**

Run:

```bash
git add tools/remo-tart/src/remo_tart/status.py tools/remo-tart/src/remo_tart/doctor.py tools/remo-tart/tests/test_status.py tools/remo-tart/tests/test_doctor.py .tart/provision.sh .tart/packs
git commit -m "refactor: make tart workspace state vm-local"
```

---

## Chunk 5: Documentation And End-To-End Verification

### Task 8: Update contributor documentation

**Files:**
- Modify: `README.md`
- Modify: `docs/tart-development-guide.md`
- Modify: `docs/tart-dev-vm.md`
- Modify: `skills/tart-dev-management/SKILL.md`

- [ ] **Step 1: Update README contributor workflow**

Replace language that says new worktrees are attached/mounted. The recommended flow should say:

```bash
brew install cirruslabs/cli/tart astral-sh/uv/uv
uv tool install --editable tools/remo-tart
remo-tart up

# inside the VM:
cd ~/Developer
git clone git@github-yjmeqt:yjmeqt/Remo.git Remo
cd Remo
make setup
```

Mention that source is not live-shared from the host by default.

- [ ] **Step 2: Update `docs/tart-development-guide.md`**

Rewrite these sections:

- "Why Remo Uses Tart For Contributor Development"
- "First-Time Setup After Clone"
- "Attaching a New Worktree"
- "Connecting Without Re-Attaching"
- "Cleaning Up a Worktree"
- "Running Tests Inside the VM"

New framing:

- `remo-tart` creates/starts/provisions VM.
- `~/Developer` is created empty.
- User clones source inside VM.
- VM-native worktrees can be created inside the VM after cloning.
- `clean-worktree` is legacy-only and not part of daily workflow.

- [ ] **Step 3: Update `docs/tart-dev-vm.md` architecture reference**

Remove or mark obsolete:

- mount manifest as the normal workspace authority
- Git root bridge as current behavior
- mount changes requiring restart as normal workflow
- host worktree `.git` rewriting

Add:

- VM-local workspace path: `~/Developer`
- provisioning transport: host reads `.tart` scripts and streams generated script into guest
- legacy mount manifests may exist but are not required

- [ ] **Step 4: Update `skills/tart-dev-management/SKILL.md`**

Replace "attach a new worktree" workflow with:

```bash
remo-tart up cursor
# in VM terminal or editor:
cd ~/Developer
git clone git@github-yjmeqt:yjmeqt/Remo.git Remo
```

Make clear Claude login and Git credentials happen inside the VM.

- [ ] **Step 5: Run documentation search**

Run:

```bash
rg -n "attach|mounted|mount|/Volumes/My Shared Files|DerivedData|clean-worktree|worktree" README.md docs/tart-development-guide.md docs/tart-dev-vm.md skills/tart-dev-management/SKILL.md
```

Expected: remaining matches either describe legacy migration/troubleshooting or VM-native Git worktrees, not the default host-mounted workspace model.

- [ ] **Step 6: Commit docs**

Run:

```bash
git add README.md docs/tart-development-guide.md docs/tart-dev-vm.md skills/tart-dev-management/SKILL.md
git commit -m "docs: document vm-owned tart workspace"
```

### Task 9: Full verification

**Files:**
- No new edits unless verification exposes issues.

- [ ] **Step 1: Run Python formatting and lint**

Run:

```bash
cd tools/remo-tart
uv run ruff format --check .
uv run ruff check .
```

Expected: pass.

- [ ] **Step 2: Run `remo-tart` unit tests**

Run:

```bash
cd tools/remo-tart
uv run pytest -v
```

Expected: pass.

- [ ] **Step 3: Run repository Rust pre-commit checks**

Run:

```bash
cargo fmt --check
cargo clippy --workspace -- -D warnings
```

Expected: pass.

- [ ] **Step 4: Run final behavior searches**

Run:

```bash
rg -n "WorktreeAttachment|guest_path_for_worktree|_normalize_worktree_gitdirs|/Volumes/My Shared Files|REMO_TART_DERIVED_DATA|CARGO_TARGET_DIR|npm_config_cache" tools/remo-tart .tart README.md docs/tart-development-guide.md docs/tart-dev-vm.md skills/tart-dev-management/SKILL.md
```

Expected:

- No code references to removed worktree path helpers.
- No default workflow docs telling users to open host-mounted source.
- `/Volumes/My Shared Files` may remain only in legacy migration/troubleshooting text if intentionally kept.

- [ ] **Step 5: Optional manual VM smoke test**

Run only when a Tart VM test is acceptable on this machine:

```bash
remo-tart destroy --force
remo-tart up cli
```

Inside the VM:

```bash
test -d ~/Developer
pwd
```

Expected:

- `~/Developer` exists.
- `remo-tart up vscode` or `remo-tart up cursor` opens `/Users/admin/Developer`.

- [ ] **Step 6: Final commit if verification fixes were needed**

If verification required small fixes:

```bash
git add -A
git commit -m "fix: complete vm-owned tart workspace migration"
```

Otherwise no commit is needed.
