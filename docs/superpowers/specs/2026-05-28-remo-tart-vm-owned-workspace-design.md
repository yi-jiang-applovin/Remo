# Remo Tart VM-Owned Workspace Design

## Goal

Simplify `remo-tart` so it creates and connects to a ready macOS development VM
without sharing the host worktree as the active workspace.

The VM should own its source checkout, build products, and caches. `remo-tart`
should create an empty development directory in the VM and leave repository
clone/init decisions to the user or agent inside the VM.

## Scope

- Stop using a Tart shared host worktree as the normal VS Code/Cursor workspace.
- Keep `remo-tart` responsible for VM lifecycle, SSH setup, editor connection,
  and toolchain provisioning.
- Create an empty `~/Developer` directory inside the VM during provisioning.
- Open `~/Developer` for `remo-tart up vscode` and `remo-tart up cursor`.
- Enter a normal VM shell for `remo-tart up cli`.
- Ship Tart pack and provision script contents from the host into the VM without
  mounting the host repository.
- Keep generated state such as Xcode DerivedData, Cargo target output, npm
  cache, and temporary files on the VM filesystem by default.
- Update contributor docs to explain that users clone repositories inside the VM.

## Non-Goals

- Do not automatically clone the current host repository.
- Do not infer or create `~/Developer/Remo`.
- Do not synchronize uncommitted host changes into the VM.
- Do not preserve the current host-worktree mount behavior as the default
  workspace path.
- Do not build a bidirectional file sync layer.
- Do not run repository build verification during `remo-tart up`; the VM may not
  contain a cloned repository yet.

## Current Behavior

The current `remo-tart` flow mounts the host repository root into Tart and opens
the selected host worktree under `/Volumes/My Shared Files/...` inside the VM.
The implementation also contains worktree-specific path handling and Git
metadata normalization so host worktrees can be used from the guest.

This creates fragile coupling between:

- host paths and guest paths
- Git linked-worktree metadata
- Tart directory sharing behavior
- Xcode generated state
- editor Remote SSH workspace paths

The most visible failure mode is generated build state under a shared worktree,
especially Xcode DerivedData, where symlink and path behavior can break or become
hard to reason about.

## Target Behavior

`remo-tart up` should:

1. Resolve and load `.tart/project.toml`.
2. Ensure the configured VM exists and is running.
3. Configure VM resources and network from project config.
4. Configure SSH and editor access.
5. Run enabled provisioning packs and the project provision hook by sending
   their contents into the VM.
6. Ensure `~/Developer` exists inside the VM.
7. Connect according to the requested mode:
   - `cli`: SSH into the VM.
   - `vscode`: open VS Code Remote SSH at `~/Developer`.
   - `cursor`: open Cursor Remote SSH at `~/Developer`.

`remo-tart use` should remain the non-connecting ensure-ready command. It should
create/start/provision the VM and ensure `~/Developer` exists, but it should not
accept or attach a host worktree path as part of normal behavior.

`remo-tart connect` should remain a lightweight connect-only command. If the VM
is not running, it should keep the existing "run `remo-tart up`" hint. It does
not need to provision the VM.

After connection, the user can run commands such as:

```bash
cd ~/Developer
git clone <origin-url> Remo
```

That checkout is VM-native. Any later Git worktrees are also VM-native and use
normal Git path semantics.

## Design Decisions

### Workspace ownership

The VM owns the development workspace. `remo-tart` does not treat the host
worktree as source of truth after VM creation.

This removes the need to translate host worktree paths into guest paths or
rewrite `.git` indirection for linked worktrees.

### Development root

The only directory `remo-tart` should create for source work is:

```text
~/Developer
```

It should not create a project-specific subdirectory. The user controls clone
name, directory layout, and branch/worktree strategy inside the VM.

### Generated state

Generated state should not be stored under a Tart shared source mount. Default
tool behavior is acceptable when it writes to VM-local locations, such as:

- Xcode DerivedData under `~/Library/Developer/Xcode/DerivedData`
- Cargo output under a VM-local checkout's `target/` or configured VM-local cache
- npm cache under the VM user's normal npm cache
- temp files under VM-local `/tmp` or `$TMPDIR`

Tart pack environment exports that point generated state into `.tart/` under a
shared worktree should be removed or replaced with VM-local paths only if the
project still needs explicit cache locations.

### Provisioning transport

The `.tart` directory remains host-side project configuration, but it should no
longer be made available to the guest through a source mount.

Instead, `remo-tart` should read these files on the host:

- `.tart/packs/_lib.sh`
- each enabled `.tart/packs/<name>.sh`
- `.tart/provision.sh`

Then it should execute them inside the VM by shipping script text over
`tart exec`, for example through a new `vm.exec_script()` helper that runs
`bash -s` with stdin.

This keeps the guest provisioning contract explicit:

- pack and provision content comes from the host checkout running `remo-tart`
- source workspaces do not need to be mounted
- the guest does not need a cloned Remo repo before provisioning can complete

### Project provision hook contract

`.tart/provision.sh` should be treated as a VM bootstrap hook, not as a command
that runs from a cloned repository.

The current Remo hook runs `make setup`, which assumes a checked-out repository
and only configures Git hooks. Under the new model, that should be removed from
VM bootstrap. Users can run `make setup` inside their VM clone after cloning if
they want Git hooks there.

### Optional host sharing

Normal development should not depend on a host source mount.

If a future escape hatch is needed, it should be explicit and clearly separate
from the normal workspace, for example a temporary import/export mount. That is
outside this design's implementation scope.

## Component Changes

### Worktree orchestration

Remove or bypass host worktree attach logic from the default `up`, `use`,
`connect`, and editor-opening path. The implementation should no longer need to:

- write a mount manifest for the host repo root
- compare running Tart mount bindings for workspace correctness
- normalize linked-worktree `.git` files
- compute guest paths from host worktree paths

### VM lifecycle

Keep the state machine for VM existence and running status if useful, but make
it independent of host workspace mounts. Mount mismatch should no longer drive
normal restart decisions.

### Provisioning

Provisioning should still source enabled packs, run pack ensure functions, and
run `.tart/provision.sh`.

Provisioning should not source those files from a guest-mounted checkout.
Instead, the host `remo-tart` process should compose or stream the provisioning
script into the guest.

Provisioning should also ensure `~/Developer` exists. This should happen in the
generated guest provisioning script so it is independent of any optional pack.

The `.tart/verify-worktree.sh` hook should not run as part of `up` or `use`
because there may be no repository clone in the VM yet.

For this implementation, keep the `verify_worktree` config field for schema
compatibility, but do not execute it from the default VM bootstrap path. Any
future removal of that field should be a separate compatibility cleanup.

### Editor connection

Editor connection should use a VM-local path:

```text
/Users/<guest_user>/Developer
```

The connection code should not require a `WorktreeAttachment` containing host
and guest worktree paths.

### Documentation

Update `README.md`, `docs/tart-development-guide.md`,
`docs/tart-dev-vm.md`, and `skills/tart-dev-management/SKILL.md` to describe
the new model:

- `remo-tart` creates a dev VM and empty `~/Developer`.
- Users clone repositories inside the VM.
- Source is not live-shared from the host by default.
- Build products and caches live on the VM filesystem.

## Error Handling

- If `~/Developer` cannot be created, provisioning should fail with a clear
  error and a next-step hint.
- If a configured pack file is missing on the host, provisioning should fail
  before attempting to run a partial guest script.
- If the host-side provision script fails inside the VM, surface the exit code
  and make clear that the failure came from VM bootstrap, not from repository
  verification.
- If VS Code or Cursor cannot open the Remote SSH URI, preserve the existing
  command exit behavior.
- If the VM is not running for `connect`, keep the existing hint to run
  `remo-tart up`.
- If `remo-tart use` receives a path argument from the old workflow, fail with a
  clear message that host worktree attach has been removed and that source
  should be cloned inside the VM under `~/Developer`.
- If old host mount state exists from previous versions, new `up` should not
  rely on it. Cleanup can be documented or handled as a migration step.

## Migration

Existing VMs may still have source mounts, mount manifests, and worktree-derived
SSH/editor paths from the previous workflow.

The implementation should prefer a simple migration:

1. New `up` no longer writes or depends on host source mounts.
2. New editor connections open `~/Developer`.
3. Docs tell users that old host-mounted workspaces are obsolete.
4. Users can run `remo-tart destroy --force && remo-tart up` if they want a
   fully clean VM.

## Verification

- Unit tests cover that `up` does not pass a host worktree attachment into the
  editor path.
- Unit tests cover that VS Code/Cursor open `/Users/<guest_user>/Developer`.
- Unit tests cover that provisioning ships pack/provision contents into the VM
  without requiring a Tart source mount.
- Unit tests cover that provisioning includes creation of `~/Developer`.
- Unit tests cover that missing configured pack files fail clearly.
- Unit tests cover that `verify_worktree` is not run during normal `up`.
- Repository search confirms default workflow docs no longer describe host
  worktree sharing as the normal model.
- Existing `tools/remo-tart` tests pass.
- If available, a manual smoke test confirms:
  - `remo-tart up cli` reaches the VM shell.
  - `~/Developer` exists in the VM.
  - `remo-tart up vscode` or `remo-tart up cursor` opens `~/Developer`.

## Risks

- Contributors may expect host edits to appear immediately in the VM. Docs must
  make the new Git-based workflow explicit.
- Existing VM state may confuse users during migration if old mounts are still
  visible. The docs should recommend a clean recreate when behavior looks mixed.
- Private repository clones inside the VM require credentials to be configured
  in the VM. This is intentional, but the docs should mention it.
