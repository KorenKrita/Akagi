"""Regression tests for ``_libriichi_loader``.

Covers the target-resolution logic so the macOS-on-Linux-binary bug
cannot silently regress: the loader must pick the right
``libriichi-{pyver}-{target}.{ext}`` for whichever (system, machine,
python_version) it is asked about. The actual ``load()`` is not
exercised here — it would require a matching prebuilt binary for the
test interpreter — but ``_resolve_target`` and ``_candidate_path``
fully determine which file ``load()`` would open, so testing them is
sufficient to catch the regression.

Per CLAUDE.md guideline 8, no real game data is touched.
"""
from __future__ import annotations

import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

LOADER_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(LOADER_DIR))

import _libriichi_loader  # noqa: E402


@pytest.mark.parametrize(
    "system,machine,expected_target,expected_ext",
    [
        ("Darwin",  "arm64",   "aarch64-apple-darwin",     ".so"),
        ("Darwin",  "aarch64", "aarch64-apple-darwin",     ".so"),
        ("Darwin",  "x86_64",  "x86_64-apple-darwin",      ".so"),
        ("Linux",   "x86_64",  "x86_64-unknown-linux-gnu", ".so"),
        ("Windows", "AMD64",   "x86_64-pc-windows-msvc",   ".pyd"),
    ],
)
def test_resolve_target(monkeypatch, system, machine, expected_target, expected_ext):
    monkeypatch.setattr("platform.system", lambda: system)
    monkeypatch.setattr("platform.machine", lambda: machine)
    target, ext = _libriichi_loader._resolve_target()
    assert target == expected_target
    assert ext == expected_ext


def test_resolve_target_unsupported(monkeypatch):
    monkeypatch.setattr("platform.system", lambda: "FreeBSD")
    monkeypatch.setattr("platform.machine", lambda: "amd64")
    with pytest.raises(RuntimeError, match="Unsupported platform"):
        _libriichi_loader._resolve_target()


@pytest.mark.parametrize(
    "system,machine,pyver,expected_filename",
    [
        ("Darwin",  "arm64",  (3, 12), "libriichi-3.12-aarch64-apple-darwin.so"),
        ("Darwin",  "x86_64", (3, 11), "libriichi-3.11-x86_64-apple-darwin.so"),
        ("Linux",   "x86_64", (3, 10), "libriichi-3.10-x86_64-unknown-linux-gnu.so"),
        ("Windows", "AMD64",  (3, 12), "libriichi-3.12-x86_64-pc-windows-msvc.pyd"),
    ],
)
def test_candidate_path(monkeypatch, tmp_path, system, machine, pyver, expected_filename):
    monkeypatch.setattr("platform.system", lambda: system)
    monkeypatch.setattr("platform.machine", lambda: machine)
    # ``sys.version_info`` is a structseq and cannot be instantiated;
    # the loader only reads ``.major``/``.minor`` so a SimpleNamespace
    # stand-in is sufficient.
    monkeypatch.setattr(
        sys, "version_info",
        SimpleNamespace(major=pyver[0], minor=pyver[1]),
    )
    candidate = _libriichi_loader._candidate_path(tmp_path)
    assert candidate == tmp_path / "libriichi" / expected_filename


def test_real_binary_present_for_current_platform():
    """Sanity: a binary exists in ``libriichi/`` for at least one
    supported Python minor on the current OS+arch. Catches release-zip
    layout drift (e.g. someone renames the binaries without updating
    the loader)."""
    target, ext = _libriichi_loader._resolve_target()
    libdir = LOADER_DIR / "libriichi"
    matches = list(libdir.glob(f"libriichi-*-{target}{ext}"))
    assert matches, (
        f"No prebuilt libriichi-*-{target}{ext} in {libdir}; "
        "release-zip layout may have drifted."
    )
