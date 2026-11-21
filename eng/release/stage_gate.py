#!/usr/bin/env python3
"""Package-verification gate for a group release.

Why this exists
---------------
Every crate in this repository shares one version, and `neotales-config-keepass`
depends on `neotales-config` at that version. Cargo resolves a dependency from
the registry while it prepares a package for upload, so the provider cannot be
packaged until the core is on crates.io. The release used to learn that only
*after* uploading the core, which leaves the group half-released: a crates.io
version is immutable, so the provider could never be published at that version
and the tag was spent.

This gate stages the exact artifacts a release would upload in a local Cargo
registry and packages every member against it, in dependency order, before
anything is uploaded. The staged registry is Cargo's own `local-registry` source
kind: a directory of `.crate` files and an index, not a stub that pretends to be
a registry. Dependencies come from the committed `Cargo.lock` and are checked
against its checksums; the participating members come from `cargo package`, the
same command `cargo publish` runs. The verification is offline, with crates.io
replaced by the staging directory, so a failure here is a packaging failure and
never a network failure.

What it proves: each member packages, and each member builds from that package
against the members before it in the form crates.io will receive.

What it does not prove: crate-name ownership, the trusted publisher configured
on crates.io, or that the registry will accept the upload. Those still fail in
the publish job, which is why that job also compares the packaged artifact
against what crates.io already holds before it decides to skip anything.

Usage
-----
    stage_gate.py gate [--root DIR] [--self-test]

        Package and verify every member in dependency order. Leaves the
        release artifacts in target/package/. `--self-test` additionally
        injects the failures this gate exists to catch, and checks the three
        verdicts of `state`, without uploading anything.

    stage_gate.py state --manifest PATH --artifact-dir DIR

        Print one word describing whether crates.io already holds the version
        in PATH at the exact bytes in the artifact: `absent`, `identical` or
        `different`. `different` means a version is already taken and the
        release must not be retried at that version.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

# Cargo's default sparse index. The publish job's recovery check reads the same
# index; nothing here talks to the crates.io API and nothing here authenticates.
INDEX_BASE = "https://index.crates.io/"

# Package directories in dependency order. The gate packages each one after the
# previous ones are in the staged registry, so this list is also the release
# order: `config/` is `neotales-config`, `keepass/` is
# `neotales-config-keepass`. A new member is added here in the same commit that
# adds it to the workspace, exactly like the map in the version job.
MEMBERS = ("config", "keepass")

DEFAULT_WORK = Path("target") / "release-gate"


class GateError(Exception):
    """A failure that should stop the release, carrying what to do about it."""


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Package a group release against a staged registry "
        "before uploading."
    )
    subcommands = parser.add_subparsers(dest="command", required=True)

    gate = subcommands.add_parser("gate", help="package and verify every member")
    gate.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="repository root, default: the checkout this script is in",
    )
    gate.add_argument(
        "--index-base",
        default=INDEX_BASE,
        help="registry index used to stage the locked dependencies",
    )
    gate.add_argument(
        "--self-test",
        action="store_true",
        help="inject the failures the gate exists to catch and check they fail",
    )

    state = subcommands.add_parser(
        "state", help="report whether crates.io already holds this exact artifact"
    )
    state.add_argument(
        "--manifest",
        type=Path,
        required=True,
        help="crate manifest that carries the version being released",
    )
    state.add_argument(
        "--artifact-dir",
        type=Path,
        required=True,
        help="directory holding <name>-<version>.crate, normally target/package",
    )
    state.add_argument(
        "--index-base",
        default=INDEX_BASE,
        help="registry index to ask; file:// is accepted for the self-test",
    )

    arguments = parser.parse_args(argv)

    try:
        if arguments.command == "gate":
            gate_release(
                arguments.root.resolve(),
                arguments.index_base.rstrip("/") + "/",
                self_test=arguments.self_test,
            )
        else:
            print(
                artifact_state(
                    arguments.manifest,
                    arguments.artifact_dir,
                    arguments.index_base.rstrip("/") + "/",
                )
            )
    except GateError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


# --------------------------------------------------------------------------
# gate
# --------------------------------------------------------------------------


def gate_release(root: Path, index_base: str, *, self_test: bool) -> None:
    target = target_dir(root)
    work = root / DEFAULT_WORK
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)

    print(f"staging registry in {work}")

    # The lockfile names the graph the release tests ran against. Fetch first so
    # a cold runner has the archives; a warm one is a no-op.
    run(["cargo", "fetch", "--locked"], root)

    registry = work / "registry"
    (registry / "index").mkdir(parents=True)
    staged = stage_locked(registry, registry / "index", root, index_base)
    print(f"staged {staged} locked dependencies")

    config = work / "cargo-config.toml"
    config.write_text(source_config(registry), encoding="utf-8")

    artifacts: list[tuple[str, str, Path, str]] = []
    for directory in MEMBERS:
        name, version = manifest_identity(root / directory / "Cargo.toml")
        print(f"\n== {name} {version}")
        print(f"   package and verify against the staged registry ({directory}/)")

        # `--offline` is deliberate: crates.io is replaced by the staging
        # directory, so resolution cannot reach the network. A failure is a
        # packaging failure, not a flake. This is the verification build
        # `cargo publish` performs, with default features.
        run(
            [
                "cargo",
                "package",
                "--package",
                name,
                "--locked",
                "--offline",
                "--config",
                str(config),
            ],
            root,
        )

        artifact = target / "package" / f"{name}-{version}.crate"
        if not artifact.is_file():
            raise GateError(
                f"cargo package did not leave {artifact}; the artifact the upload "
                "would use cannot be inspected"
            )
        digest = sha256(artifact)
        stage_artifact(
            registry, registry / "index", work / "unpack", artifact, name, version
        )
        print(f"   staged {artifact.name} sha256 {digest}")
        artifacts.append((name, version, artifact, digest))

    print(
        "\ngate passed: every member packages and verifies against the candidate "
        "artifacts. Nothing was uploaded and no registry token was read."
    )

    if self_test:
        self_check(work, root, artifacts)


def stage_locked(registry: Path, index_root: Path, root: Path, index_base: str) -> int:
    """Copy every locked registry crate into the staging registry.

    The checksum in `Cargo.lock` is the integrity check: a cache entry that does
    not hash to it is not the crate the release tested against.
    """
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    cache_dirs = sorted((cargo_home() / "registry" / "cache").glob("*"))
    if not cache_dirs:
        raise GateError(
            "no Cargo download cache; `cargo fetch --locked` should have created one"
        )

    documents: dict[str, str] = {}
    count = 0
    for package in lock["package"]:
        if not str(package.get("source", "")).startswith("registry+"):
            continue
        name = package["name"]
        version = package["version"]
        expected = package.get("checksum")
        if not expected:
            raise GateError(
                f"{name} {version} has a registry source but no checksum in Cargo.lock"
            )

        archive = find_cached(cache_dirs, name, version)
        actual = sha256(archive)
        if actual != expected:
            raise GateError(
                f"{archive.name} hashes to {actual}, Cargo.lock says {expected}. "
                "The download cache is not the graph the lockfile describes; "
                "clear ~/.cargo/registry/cache and retry."
            )

        shutil.copy2(archive, registry / archive.name)

        if name not in documents:
            documents[name] = fetch_index(name, index_base)
        line = index_line(documents[name], name, version)
        if line is None:
            raise GateError(
                f"{name} {version} is in Cargo.lock but not in the registry index at "
                f"{index_base}. The lockfile and the registry disagree, so the "
                "release cannot be staged."
            )
        append_index(index_root, name, line)
        count += 1
    return count


def stage_artifact(
    registry: Path,
    index_root: Path,
    unpack_root: Path,
    artifact: Path,
    name: str,
    version: str,
) -> None:
    """Add a freshly packaged member to the staging registry.

    Its index entry is derived from the normalized manifest inside the archive,
    which is the input the registry's own entry is generated from, and its
    checksum is the hash of these exact bytes.
    """
    shutil.copy2(artifact, registry / artifact.name)

    if unpack_root.exists():
        shutil.rmtree(unpack_root)
    unpack_root.mkdir(parents=True)
    with tarfile.open(artifact, "r:gz") as archive:
        if hasattr(tarfile, "data_filter"):
            archive.extractall(unpack_root, filter="data")
        else:  # pragma: no cover - Python older than 3.12
            archive.extractall(unpack_root)

    manifest = unpack_root / f"{name}-{version}" / "Cargo.toml"
    entry = index_entry(
        name=name,
        version=version,
        manifest=tomllib.loads(manifest.read_text(encoding="utf-8")),
        checksum=sha256(artifact),
    )
    append_index(index_root, name, json.dumps(entry, separators=(",", ":")))


def index_entry(*, name: str, version: str, manifest: dict, checksum: str) -> dict:
    """Build a registry index entry from a packaged, normalized manifest."""
    package = manifest.get("package")
    if not isinstance(package, dict):
        raise GateError(f"packaged {name} {version} has no [package] table")

    dependencies = []
    tables = (
        ("normal", "dependencies"),
        ("build", "build-dependencies"),
        ("dev", "dev-dependencies"),
    )
    targets = [(None, manifest)] + [
        (configuration, table)
        for configuration, table in (manifest.get("target") or {}).items()
    ]
    for configuration, table in targets:
        for kind, key in tables:
            for dependency_name, spec in (table.get(key) or {}).items():
                for one in spec if isinstance(spec, list) else [spec]:
                    if isinstance(one, str):
                        one = {"version": one}
                    requirement = one.get("version")
                    if not isinstance(requirement, str) or not requirement:
                        raise GateError(
                            f"{name} {version} dependency {dependency_name} has no "
                            "version; a path-only dependency cannot be published"
                        )
                    entry = {
                        "name": dependency_name,
                        "req": requirement
                        if not requirement[0].isdigit()
                        else f"^{requirement}",
                        "features": sorted(one.get("features") or []),
                        "optional": bool(one.get("optional", False)),
                        "default_features": bool(one.get("default-features", True)),
                        "target": configuration,
                        "kind": kind,
                    }
                    if "package" in one:
                        entry["package"] = one["package"]
                    dependencies.append(entry)

    entry = {
        "name": name,
        "vers": version,
        "deps": dependencies,
        "cksum": checksum,
        "features": manifest.get("features") or {},
        "yanked": False,
    }
    rust_version = package.get("rust-version")
    if isinstance(rust_version, str):
        entry["rust_version"] = rust_version
    if "links" in package:
        entry["links"] = package["links"]
    return entry


# --------------------------------------------------------------------------
# state: is this version already published, and are the bytes the same?
# --------------------------------------------------------------------------


def artifact_state(manifest: Path, artifact_dir: Path, index_base: str) -> str:
    name, version = manifest_identity(manifest)
    artifact = artifact_dir / f"{name}-{version}.crate"
    if not artifact.is_file():
        raise GateError(
            f"{artifact} is missing. Run `stage_gate.py gate` first: the state "
            "check compares the artifact this run would upload, never a version "
            "string alone."
        )
    local = sha256(artifact)
    line = index_line(fetch_index(name, index_base), name, version)
    if line is None:
        print(
            f"{name} {version} is not published; {artifact.name} sha256 {local} "
            "would be a new upload",
            file=sys.stderr,
        )
        return "absent"

    published = json.loads(line).get("cksum")
    if published == local:
        print(
            f"crates.io already holds {name} {version} at sha256 {local}, the exact "
            f"artifact in {artifact.name}",
            file=sys.stderr,
        )
        return "identical"

    print(
        f"crates.io holds {name} {version} at sha256 {published}, but "
        f"{artifact.name} hashes to {local}",
        file=sys.stderr,
    )
    return "different"


# --------------------------------------------------------------------------
# self-test: the failures this gate exists to catch
# --------------------------------------------------------------------------


def self_check(
    work: Path, root: Path, artifacts: list[tuple[str, str, Path, str]]
) -> None:
    """Inject the failures the gate must catch, and the verdicts `state` returns.

    These are the release's own failure paths, so they are exercised on every
    CI run rather than trusted. Nothing is uploaded and no token is read.
    """
    core_name, core_version, core_artifact, _ = artifacts[0]
    last_name = artifacts[-1][0]

    print("\n== self-test: failure injection")

    # 1. The pre-fix blocker: the core is not in the registry at all.
    missing = copy_registry(work / "registry", work / "inject-missing-core")
    (missing / core_artifact.name).unlink()
    remove_index(missing / "index", core_name)

    output = expect_failure(
        package_command(work, last_name, missing),
        description=f"packaging {last_name} without {core_name} in the registry",
    )
    assert_message(
        output, core_name, description="the error names the missing dependency"
    )

    # 2. A version the provider does not require: the group versions drifted.
    drifted_version = bump_major(core_version)
    drifted = copy_registry(work / "registry", work / "inject-version-drift")
    rename_index_version(drifted / "index", core_name, core_version, drifted_version)
    (drifted / core_artifact.name).rename(
        drifted / f"{core_name}-{drifted_version}.crate"
    )

    output = expect_failure(
        package_command(work, last_name, drifted),
        description=f"packaging {last_name} against {core_name} {drifted_version}",
    )
    assert_message(
        output,
        core_name,
        description="the error names the requirement it could not satisfy",
    )

    # 3. The recovery verdicts, against a local index built from the staged
    #    entry, so the comparison path is tested without publishing anything.
    index = work / "inject-state-index"
    staged_line = index_line(
        (work / "registry" / "index" / index_path(core_name)).read_text(
            encoding="utf-8"
        ),
        core_name,
        core_version,
    )
    if staged_line is None:
        raise GateError(f"self-test: the staged index has no entry for {core_name}")
    append_index(index, core_name, staged_line)

    base = index.resolve().as_uri().rstrip("/") + "/"
    manifest = root / member_directory(root, core_name) / "Cargo.toml"
    artifact_dir = target_dir(root) / "package"

    expect_state(
        "identical",
        artifact_state(manifest, artifact_dir, base),
        description="an artifact that matches the published checksum",
    )

    rewrite_checksum(index, core_name, "0" * 64)
    expect_state(
        "different",
        artifact_state(manifest, artifact_dir, base),
        description="an artifact whose bytes differ from the published checksum",
    )

    remove_index(index, core_name)
    expect_state(
        "absent",
        artifact_state(manifest, artifact_dir, base),
        description="a version the index does not list",
    )

    print(
        "self-test passed: the gate rejects a missing or mismatched core, and "
        "state reports absent, identical and different"
    )


def package_command(work: Path, name: str, registry: Path) -> list[str]:
    config = work / f"cargo-config-{registry.name}.toml"
    config.write_text(source_config(registry), encoding="utf-8")
    return [
        "cargo",
        "package",
        "--package",
        name,
        "--locked",
        "--offline",
        "--config",
        str(config),
    ]


def expect_failure(command: list[str], *, description: str) -> str:
    result = subprocess.run(
        command,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    output = result.stdout + result.stderr
    if result.returncode == 0:
        raise GateError(
            f"self-test: {description} succeeded, but the gate must reject it"
        )
    print(f"   ok: {description} fails as it must")
    return output


def assert_message(output: str, needle: str, *, description: str) -> None:
    if needle not in output:
        raise GateError(
            f"self-test: {description} did not hold; expected {needle!r} in:\n{output}"
        )
    print(f"   ok: {description}")


def expect_state(expected: str, actual: str, *, description: str) -> None:
    if expected != actual:
        raise GateError(
            f"self-test: {description} reported {actual!r}, expected {expected!r}"
        )
    print(f"   ok: {description} reports {expected}")


def copy_registry(source: Path, destination: Path) -> Path:
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(source, destination)
    return destination


def bump_major(version: str) -> str:
    release = version.split("-", 1)[0].split("+", 1)[0]
    major = int(release.split(".")[0])
    return f"{major + 1}.0.0"


def member_directory(root: Path, name: str) -> str:
    for directory in MEMBERS:
        if manifest_identity(root / directory / "Cargo.toml")[0] == name:
            return directory
    raise GateError(f"{name} is not one of the release members {MEMBERS}")


# --------------------------------------------------------------------------
# registry plumbing
# --------------------------------------------------------------------------


def source_config(registry: Path) -> str:
    return (
        "[source.crates-io]\n"
        'replace-with = "staged"\n'
        "\n"
        "[source.staged]\n"
        f'local-registry = "{registry}"\n'
    )


def index_path(name: str) -> str:
    """The registry's directory layout for a crate name."""
    if len(name) == 1:
        return f"1/{name}"
    if len(name) == 2:
        return f"2/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def fetch_index(name: str, index_base: str) -> str:
    """Fetch a crate's index document, retrying the transport only."""
    url = index_base + index_path(name)
    last: Exception | None = None
    for attempt in range(3):
        try:
            with urllib.request.urlopen(url, timeout=30) as response:
                return response.read().decode("utf-8")
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return ""
            last = error
        except urllib.error.URLError as error:
            # A local file index reports a missing document this way. A crate
            # with no versions is the same thing to the caller.
            if isinstance(error.reason, (FileNotFoundError, NotADirectoryError)):
                return ""
            last = error
        if attempt < 2:
            time.sleep(2 * (attempt + 1))
    raise GateError(f"cannot read {url}: {last}")


def index_line(document: str, name: str, version: str) -> str | None:
    for line in document.splitlines():
        line = line.strip()
        if not line:
            continue
        entry = json.loads(line)
        if entry.get("name") == name and entry.get("vers") == version:
            return line
    return None


def append_index(index_root: Path, name: str, line: str) -> None:
    path = index_root / index_path(name)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(line + "\n")


def remove_index(index_root: Path, name: str) -> None:
    path = index_root / index_path(name)
    if path.exists():
        path.unlink()


def rewrite_checksum(index_root: Path, name: str, checksum: str) -> None:
    path = index_root / index_path(name)
    lines = []
    for line in path.read_text(encoding="utf-8").splitlines():
        entry = json.loads(line)
        entry["cksum"] = checksum
        lines.append(json.dumps(entry, separators=(",", ":")))
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def rename_index_version(index_root: Path, name: str, old: str, new: str) -> None:
    path = index_root / index_path(name)
    lines = []
    for line in path.read_text(encoding="utf-8").splitlines():
        entry = json.loads(line)
        if entry.get("vers") == old:
            entry["vers"] = new
        lines.append(json.dumps(entry, separators=(",", ":")))
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


# --------------------------------------------------------------------------
# helpers
# --------------------------------------------------------------------------


def manifest_identity(manifest: Path) -> tuple[str, str]:
    document = tomllib.loads(manifest.read_text(encoding="utf-8"))
    package = document.get("package")
    if not isinstance(package, dict):
        raise GateError(f"{manifest} has no [package] table")
    name = package.get("name")
    version = package.get("version")
    if not isinstance(name, str) or not isinstance(version, str):
        raise GateError(
            f"{manifest} must carry a literal package name and version; a "
            "workspace-inherited version cannot be released from a tag"
        )
    return name, version


def cargo_home() -> Path:
    return Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")


def target_dir(root: Path) -> Path:
    configured = os.environ.get("CARGO_TARGET_DIR")
    if not configured:
        return root / "target"
    path = Path(configured)
    return path if path.is_absolute() else (root / path).resolve()


def find_cached(cache_dirs: list[Path], name: str, version: str) -> Path:
    filename = f"{name}-{version}.crate"
    for directory in cache_dirs:
        candidate = directory / filename
        if candidate.is_file():
            return candidate
    raise GateError(
        f"{filename} is not in the Cargo download cache under {cargo_home()}. "
        "`cargo fetch --locked` should have fetched it; a private registry "
        "mirror without this crate cannot stage a release."
    )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(command: list[str], cwd: Path) -> None:
    print("   $ " + " ".join(command), flush=True)
    result = subprocess.run(command, cwd=cwd)
    if result.returncode != 0:
        raise GateError(
            f"`{' '.join(command)}` exited {result.returncode}. Nothing was "
            "uploaded. Fix the package, run the gate again, and only then tag."
        )


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
