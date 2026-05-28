"""Connect dispatchers — CLI (ssh), VS Code, and Cursor."""

from __future__ import annotations

import subprocess

from remo_tart.ssh import ssh_alias


def developer_dir(guest_user: str) -> str:
    return f"/Users/{guest_user}/Developer"


def connect_cli(vm_name: str, guest_user: str) -> int:
    """Drop into an interactive SSH shell on *vm_name*."""
    del guest_user  # ssh alias already encodes the user
    alias = ssh_alias(vm_name)
    return subprocess.run(["ssh", alias], check=False).returncode


def connect_vscode(
    vm_name: str,
    guest_user: str,
    *,
    new_window: bool = False,
) -> int:
    """Open the VM-local developer directory in VS Code via SSH-remote."""
    alias = ssh_alias(vm_name)
    uri = f"vscode-remote://ssh-remote+{alias}{developer_dir(guest_user)}"
    window_flag = "--new-window" if new_window else "--reuse-window"
    argv = ["code", window_flag, "--folder-uri", uri]
    return subprocess.run(argv, check=False).returncode


def connect_cursor(
    vm_name: str,
    guest_user: str,
    *,
    new_window: bool = False,
) -> int:
    """Open the VM-local developer directory in Cursor."""
    alias = ssh_alias(vm_name)
    uri = f"vscode-remote://ssh-remote+{alias}{developer_dir(guest_user)}"
    window_flag = "--new-window" if new_window else "--reuse-window"
    argv = ["cursor", window_flag, "--folder-uri", uri]
    return subprocess.run(argv, check=False).returncode
