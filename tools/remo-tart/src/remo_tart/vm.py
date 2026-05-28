"""Tart CLI subprocess wrappers.

Every function that shells out to ``tart`` lives here.  All other modules that
need VM state or mutation import from this module rather than calling
``subprocess`` directly.

Memory note: ``tart set --memory`` expects megabytes, so ``memory_gb`` is
multiplied by 1024 before being passed to the CLI.
"""

from __future__ import annotations

import json
import subprocess

from remo_tart.errors import RemoTartError
from remo_tart.mount import MountEntry

# ---------------------------------------------------------------------------
# State queries
# ---------------------------------------------------------------------------


def list_names() -> list[str]:
    """Return names of all locally known tart VMs (``tart list --quiet``)."""
    result = subprocess.run(
        ["tart", "list", "--quiet"],
        capture_output=True,
        text=True,
        check=False,
    )
    return [line for line in result.stdout.splitlines() if line.strip()]


def exists(name: str) -> bool:
    """Return ``True`` if *name* appears in the local tart VM list."""
    return name in list_names()


def get_state(name: str) -> dict:  # type: ignore[type-arg]
    """Return the parsed JSON state dict for *name* (``tart get <name> --format json``).

    Raises :exc:`~remo_tart.errors.RemoTartError` if the tart call fails.
    """
    result = subprocess.run(
        ["tart", "get", name, "--format", "json"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RemoTartError(
            f"tart get failed for VM '{name}': {result.stderr.strip()}",
            hint=f"Run 'tart list' to see available VMs, or 'tart run {name}' to start it.",
        )
    return json.loads(result.stdout)  # type: ignore[no-any-return]


def is_running(name: str) -> bool:
    """Return ``True`` if *name* is in the ``running`` state.

    Handles both ``"State"`` and ``"state"`` keys and case-insensitive values,
    because tart has been inconsistent across versions.
    """
    state = get_state(name)
    # Accept either capitalisation of the key
    raw_value = state.get("State") or state.get("state") or ""
    return raw_value.lower() == "running"


def ip_address(name: str) -> str | None:
    """Return the IP address for *name*, or ``None`` if it cannot be determined.

    First tries ``tart ip <name>``.  If that fails, falls back to
    ``tart exec <name> -- /usr/bin/ipconfig getifaddr en0`` (mirroring the
    ``remo_tart_vm_connect_ip`` logic in ``common.sh:658-688``).
    """
    result = subprocess.run(
        ["tart", "ip", name],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0:
        ip = result.stdout.strip()
        if ip:
            return ip

    # Fallback: ask the guest directly
    fallback = subprocess.run(
        ["tart", "exec", name, "/usr/bin/ipconfig", "getifaddr", "en0"],
        capture_output=True,
        text=True,
        check=False,
    )
    if fallback.returncode == 0:
        ip = fallback.stdout.strip()
        if ip:
            return ip

    return None


# ---------------------------------------------------------------------------
# Mutation helpers (clone / delete / configure)
# ---------------------------------------------------------------------------


def create(name: str, from_image: str) -> None:
    """Clone *from_image* into a new local VM named *name*."""
    subprocess.run(
        ["tart", "clone", from_image, name],
        check=True,
    )


def delete(name: str) -> None:
    """Delete the local VM named *name*."""
    subprocess.run(
        ["tart", "delete", name],
        check=True,
    )


def set_resources(name: str, cpu: int, memory_gb: int) -> None:
    """Configure CPU count and RAM for *name*.

    ``memory_gb`` is converted to megabytes (``* 1024``) before being passed
    to ``tart set --memory``, which expects MB.
    """
    subprocess.run(
        [
            "tart",
            "set",
            name,
            "--cpu",
            str(cpu),
            "--memory",
            str(memory_gb * 1024),
        ],
        check=True,
    )


# ---------------------------------------------------------------------------
# Exec wrappers
# ---------------------------------------------------------------------------


def exec_capture(name: str, argv: list[str]) -> subprocess.CompletedProcess:  # type: ignore[type-arg]
    """Run *argv* inside *name* and capture stdout/stderr.

    Returns the :class:`subprocess.CompletedProcess` without raising on
    non-zero exit codes — callers are responsible for checking ``returncode``.

    Note: ``tart exec`` syntax is ``tart exec <vm> <cmd> [args...]`` —
    there is NO ``--`` separator (tart treats ``--`` as a command name and
    fails with ``executable file not found``).
    """
    return subprocess.run(
        ["tart", "exec", name, *argv],
        capture_output=True,
        text=True,
        check=False,
    )


def exec_interactive(name: str, argv: list[str]) -> int:
    """Run *argv* inside *name* with an inherited tty.

    Returns the exit code of the remote command.  See :func:`exec_capture`
    for the note about ``--``.
    """
    result = subprocess.run(
        ["tart", "exec", name, *argv],
        check=False,
    )
    return result.returncode


def exec_script(name: str, script: str) -> int:
    """Run *script* inside *name* via ``bash -s`` with streamed output.

    ``tart exec -i`` is required for forwarding host stdin to the guest.
    """
    result = subprocess.run(
        ["tart", "exec", "-i", name, "bash", "-s"],
        input=script,
        text=True,
        check=False,
    )
    return result.returncode


# ---------------------------------------------------------------------------
# Pure build helper (no subprocess — safe to call from launchd submitter)
# ---------------------------------------------------------------------------


def build_run_args(
    name: str,
    network: str,
    mounts: list[MountEntry],
    *,
    headless: bool = True,
) -> list[str]:
    """Return the ``tart run`` argv list for *name* (without the ``tart`` prefix).

    This is a **pure function** — it never shells out.  The launchd submitter
    calls it to compose the run command before handing it to ``launchctl``.

    By default the VM runs **headless** (``--no-graphics``); set
    ``headless=False`` to open a UI window (useful for debugging boot, GUI
    work, or running an installer that needs the display).

    Network strings:
    - ``"shared"``        → ``["--net-shared"]``
    - ``"softnet"``       → ``["--net-softnet"]``
    - ``"bridged:<iface>"`` → ``["--net-bridged", "<iface>"]``

    Each :class:`~remo_tart.mount.MountEntry` adds
    ``["--dir", "<name>:<host_path>"]``.  Tart's ``--dir`` options are
    ``ro`` or ``tag=<TAG>`` only — there is no ``rw`` option (rw is the default).
    """
    args: list[str] = ["run", name]

    if headless:
        args.append("--no-graphics")

    # Network
    if network == "shared":
        args.append("--net-shared")
    elif network == "softnet":
        args.append("--net-softnet")
    elif network.startswith("bridged:"):
        iface = network[len("bridged:") :]
        args += ["--net-bridged", iface]

    # Mounts
    for entry in mounts:
        args += ["--dir", f"{entry.name}:{entry.host_path}"]

    return args
