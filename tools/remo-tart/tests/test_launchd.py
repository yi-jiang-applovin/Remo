from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

from remo_tart.launchd import build_submit_argv, label


def test_label_uses_slug() -> None:
    assert label("remo-dev") == "com.remo.tart.remo-dev"


def test_label_slugifies_uppercase() -> None:
    assert label("Remo-Dev") == "com.remo.tart.remo-dev"


def test_build_submit_argv_produces_expected_shape(tmp_path: Path) -> None:
    log = tmp_path / "remo-dev.log"
    argv = build_submit_argv(
        label_str="com.remo.tart.remo-dev",
        tart_args=["run", "remo-dev", "--net-bridged", "en0"],
        log_path=log,
    )
    # ["launchctl", "submit", "-l", label, "--", "/bin/zsh", "-lc", "exec tart run ... > log 2>&1"]
    assert argv[0:4] == ["launchctl", "submit", "-l", "com.remo.tart.remo-dev"]
    assert argv[4] == "--"
    assert argv[5:7] == ["/bin/zsh", "-lc"]
    assert "exec tart run remo-dev --net-bridged en0" in argv[7]
    assert f"> {log}" in argv[7]
    assert "2>&1" in argv[7]


def test_job_present_true_when_launchctl_prints(tmp_path: Path) -> None:
    from remo_tart.launchd import job_present

    with patch("subprocess.run") as run:
        run.return_value.returncode = 0
        assert job_present("com.remo.tart.remo-dev") is True


def test_job_present_false_when_launchctl_fails(tmp_path: Path) -> None:
    from remo_tart.launchd import job_present

    with patch("subprocess.run") as run:
        run.return_value.returncode = 1
        assert job_present("com.remo.tart.remo-dev") is False


_LAUNCHCTL_PRINT_TEMPLATE = """\
gui/501/com.remo.tart.remo-dev = {{
\tactive count = 3
\tstate = running

\tprogram = /bin/zsh
\targuments = {{
\t\t/bin/zsh
\t\t-lc
\t\t{cmd}
\t}}
\tpid = 75144
}}
"""


def _print_stub(returncode: int, stdout: str = "") -> object:
    """Return a CompletedProcess-shaped MagicMock for subprocess.run."""
    from unittest.mock import MagicMock

    proc = MagicMock()
    proc.returncode = returncode
    proc.stdout = stdout
    return proc


def test_running_tart_argv_parses_dir_bindings() -> None:
    from remo_tart.launchd import running_tart_argv

    cmd = (
        "exec tart run pulse-ios-dev-vm --net-bridged en0 "
        "--dir pulse-ios-dev-vm:/Users/jane/Pulse "
        "> /Users/jane/.config/remo/tart/pulse-ios-dev-vm.log 2>&1"
    )
    output = _LAUNCHCTL_PRINT_TEMPLATE.format(cmd=cmd)
    with patch("subprocess.run", return_value=_print_stub(0, output)):
        argv = running_tart_argv("com.remo.tart.pulse-ios-dev-vm")
    assert argv is not None
    assert argv[:3] == ["tart", "run", "pulse-ios-dev-vm"]
    assert "--dir" in argv
    assert "pulse-ios-dev-vm:/Users/jane/Pulse" in argv
    assert ">" not in argv
    assert "2>&1" not in argv


def test_running_tart_argv_returns_none_when_job_absent() -> None:
    from remo_tart.launchd import running_tart_argv

    with patch("subprocess.run", return_value=_print_stub(1, "")):
        assert running_tart_argv("com.remo.tart.nope") is None


def test_running_tart_argv_returns_none_when_no_exec_line() -> None:
    """`launchctl print` succeeded but did not contain an `exec tart` line —
    e.g. a job submitted by something other than this module. We treat that
    as "unknown" rather than crashing or matching partial junk."""
    from remo_tart.launchd import running_tart_argv

    with patch("subprocess.run", return_value=_print_stub(0, "no exec line here")):
        assert running_tart_argv("com.remo.tart.foo") is None


def test_running_tart_argv_handles_quoted_paths() -> None:
    """Paths containing spaces are shell-quoted in the launchctl print output;
    shlex must round-trip them as single tokens."""
    from remo_tart.launchd import running_tart_argv

    cmd = (
        "exec tart run space-vm "
        "--dir space-vm:'/Users/jane/With Space/Repo' "
        "> '/log path/x.log' 2>&1"
    )
    output = _LAUNCHCTL_PRINT_TEMPLATE.format(cmd=cmd)
    with patch("subprocess.run", return_value=_print_stub(0, output)):
        argv = running_tart_argv("com.remo.tart.space-vm")
    assert argv is not None
    assert "space-vm:/Users/jane/With Space/Repo" in argv
