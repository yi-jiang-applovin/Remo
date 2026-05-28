from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock, patch

from remo_tart import vm
from remo_tart.mount import MountEntry


@patch("subprocess.run")
def test_list_names_parses_stdout(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout="remo-dev\nother\n", stderr="")
    assert vm.list_names() == ["remo-dev", "other"]


@patch("subprocess.run")
def test_list_names_strips_blank_lines(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout="\nremo-dev\n\nother\n\n", stderr="")
    assert vm.list_names() == ["remo-dev", "other"]


@patch("subprocess.run")
def test_exists_true_when_in_list(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout="remo-dev\n", stderr="")
    assert vm.exists("remo-dev") is True


@patch("subprocess.run")
def test_exists_false_when_not_in_list(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout="remo-dev\n", stderr="")
    assert vm.exists("other") is False


@patch("subprocess.run")
def test_get_state_parses_json(run: MagicMock) -> None:
    run.return_value = MagicMock(
        returncode=0,
        stdout=json.dumps({"State": "running", "CPU": 6}),
        stderr="",
    )
    assert vm.get_state("remo-dev")["State"] == "running"


@patch("subprocess.run")
def test_is_running_true(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout=json.dumps({"State": "running"}), stderr="")
    assert vm.is_running("remo-dev") is True


@patch("subprocess.run")
def test_is_running_false(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout=json.dumps({"State": "stopped"}), stderr="")
    assert vm.is_running("remo-dev") is False


@patch("subprocess.run")
def test_is_running_case_insensitive(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout=json.dumps({"state": "Running"}), stderr="")
    assert vm.is_running("remo-dev") is True


def test_build_run_args_bridged_network_and_mounts() -> None:
    mounts = [
        MountEntry("remo", Path("/r")),
        MountEntry("remo-git-root", Path("/r/.git")),
    ]
    args = vm.build_run_args("remo-dev", network="bridged:en0", mounts=mounts)
    # positional: ["run", "remo-dev"]
    assert args[0:2] == ["run", "remo-dev"]
    # network flag present
    assert "--net-bridged" in args
    idx = args.index("--net-bridged")
    assert args[idx + 1] == "en0"
    # each mount adds --dir <name>:<host-path>:rw
    assert any("remo:/r" in a for a in args)
    assert any("remo-git-root:/r/.git" in a for a in args)


def test_build_run_args_shared_network() -> None:
    args = vm.build_run_args("remo-dev", network="shared", mounts=[])
    assert "--net-shared" in args


def test_build_run_args_softnet() -> None:
    args = vm.build_run_args("remo-dev", network="softnet", mounts=[])
    assert "--net-softnet" in args


def test_build_run_args_does_not_emit_rw_option() -> None:
    """Tart's --dir options are ro/tag=, NOT rw. Adding :rw makes Tart reject
    the directory share with VZErrorDomain Code=2."""
    mounts = [MountEntry("remo", Path("/r"))]
    args = vm.build_run_args("remo-dev", network="shared", mounts=mounts)
    assert "remo:/r" in args
    assert not any(":rw" in a for a in args)


def test_build_run_args_headless_by_default() -> None:
    args = vm.build_run_args("remo-dev", network="shared", mounts=[])
    assert "--no-graphics" in args


def test_build_run_args_with_display_omits_no_graphics() -> None:
    args = vm.build_run_args("remo-dev", network="shared", mounts=[], headless=False)
    assert "--no-graphics" not in args


@patch("subprocess.run")
def test_create_invokes_tart_clone(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout="", stderr="")
    vm.create("remo-dev", "ghcr.io/org/img:tag")
    (called_argv,) = run.call_args[0]
    assert called_argv[0:3] == ["tart", "clone", "ghcr.io/org/img:tag"]
    assert called_argv[3] == "remo-dev"


@patch("subprocess.run")
def test_delete_invokes_tart_delete(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout="", stderr="")
    vm.delete("remo-dev")
    (called_argv,) = run.call_args[0]
    assert called_argv == ["tart", "delete", "remo-dev"]


@patch("subprocess.run")
def test_set_resources(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout="", stderr="")
    vm.set_resources("remo-dev", cpu=6, memory_gb=12)
    (called_argv,) = run.call_args[0]
    assert called_argv[0:3] == ["tart", "set", "remo-dev"]
    # --cpu 6 --memory 12288 (MB) or similar — check both appear
    assert "--cpu" in called_argv
    assert str(6) in called_argv
    assert "--memory" in called_argv


@patch("subprocess.run")
def test_exec_capture_returns_completed_process(run: MagicMock) -> None:
    cp = MagicMock(returncode=0, stdout="hello", stderr="")
    run.return_value = cp
    result = vm.exec_capture("remo-dev", ["echo", "hello"])
    assert result is cp
    (called_argv,) = run.call_args[0]
    assert called_argv[0:3] == ["tart", "exec", "remo-dev"]
    assert called_argv[-2:] == ["echo", "hello"]


@patch("subprocess.run")
def test_exec_capture_does_not_pass_double_dash_separator(run: MagicMock) -> None:
    """``tart exec`` does not accept ``--`` as a separator — it treats ``--``
    as a command name and fails with ``executable file not found``."""
    run.return_value = MagicMock(returncode=0, stdout="", stderr="")
    vm.exec_capture("remo-dev", ["echo", "hi"])
    (called_argv,) = run.call_args[0]
    assert "--" not in called_argv


@patch("subprocess.run")
def test_exec_interactive_does_not_pass_double_dash_separator(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0)
    vm.exec_interactive("remo-dev", ["sh"])
    (called_argv,) = run.call_args[0]
    assert "--" not in called_argv


@patch("remo_tart.vm.subprocess.run")
def test_exec_script_streams_script_to_bash_stdin(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0)

    assert vm.exec_script("remo-dev", "echo hi\n") == 0

    run.assert_called_once()
    assert run.call_args.args[0] == ["tart", "exec", "-i", "remo-dev", "bash", "-s"]
    assert run.call_args.kwargs["input"] == "echo hi\n"
    assert run.call_args.kwargs["text"] is True
    assert run.call_args.kwargs["check"] is False


@patch("subprocess.run")
def test_ip_address_returns_stdout(run: MagicMock) -> None:
    run.return_value = MagicMock(returncode=0, stdout="10.0.0.2\n", stderr="")
    assert vm.ip_address("remo-dev") == "10.0.0.2"


@patch("subprocess.run")
def test_ip_address_returns_none_when_tart_ip_fails(run: MagicMock) -> None:
    # tart ip fails (returncode != 0); fallback also fails
    run.return_value = MagicMock(returncode=1, stdout="", stderr="not running")
    assert vm.ip_address("remo-dev") is None
