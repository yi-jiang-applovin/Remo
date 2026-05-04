"""Wrap launchctl. Uses `launchctl submit` (in-memory, no plist file)."""

from __future__ import annotations

import os
import re
import shlex
import subprocess
from pathlib import Path


def _slug(name: str) -> str:
    return re.sub(r"[^a-z0-9.-]+", "-", name.lower()).strip("-")


def label(vm_name: str) -> str:
    return f"com.remo.tart.{_slug(vm_name)}"


def build_submit_argv(
    *,
    label_str: str,
    tart_args: list[str],
    log_path: Path,
) -> list[str]:
    quoted = " ".join(shlex.quote(a) for a in tart_args)
    shell_cmd = f"exec tart {quoted} > {shlex.quote(str(log_path))} 2>&1"
    return [
        "launchctl",
        "submit",
        "-l",
        label_str,
        "--",
        "/bin/zsh",
        "-lc",
        shell_cmd,
    ]


def submit(label_str: str, tart_args: list[str], log_path: Path) -> None:
    argv = build_submit_argv(label_str=label_str, tart_args=tart_args, log_path=log_path)
    subprocess.run(argv, check=True)


def remove(label_str: str) -> None:
    subprocess.run(["launchctl", "remove", label_str], check=False)


def _gui_uid() -> int:
    return os.getuid()


def job_present(label_str: str) -> bool:
    target = f"gui/{_gui_uid()}/{label_str}"
    result = subprocess.run(
        ["launchctl", "print", target],
        check=False,
        capture_output=True,
    )
    return result.returncode == 0


def running_tart_argv(label_str: str) -> list[str] | None:
    """Return the argv of the ``tart`` invocation managed by *label_str*.

    Parses the ``arguments = { ... }`` block from ``launchctl print``. The
    block always contains an ``exec tart …`` shell command line because we
    submit jobs via ``/bin/zsh -lc 'exec tart … > log 2>&1'`` (see
    :func:`build_submit_argv`). Returns ``None`` if the job is not active or
    the command line cannot be located.

    The returned list starts with ``"tart"`` (e.g. ``["tart", "run",
    "<vm>", "--dir", "<name>:<path>", ...]``); trailing shell redirection
    tokens (``> /log 2>&1``) are stripped.
    """
    target = f"gui/{_gui_uid()}/{label_str}"
    result = subprocess.run(
        ["launchctl", "print", target],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return None
    for raw in result.stdout.splitlines():
        line = raw.strip()
        if not line.startswith("exec tart "):
            continue
        try:
            tokens = shlex.split(line[len("exec ") :])
        except ValueError:
            return None
        # Strip trailing shell redirections (`> /path/log 2>&1`).
        for sentinel in ("2>&1", ">"):
            if sentinel in tokens:
                tokens = tokens[: tokens.index(sentinel)]
        return tokens
    return None


def cleanup_stale(label_str: str, vm_running: bool) -> bool:
    """If a launchd job is registered but the VM isn't actually running, remove it.

    Returns True if cleanup happened.
    """
    if vm_running:
        return False
    if not job_present(label_str):
        return False
    remove(label_str)
    return True
