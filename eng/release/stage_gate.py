#!/usr/bin/env python3
"""Package-verification and publish gate for a group release.

Why this exists
---------------
Every crate in this repository shares one version, and `neotales-config-keepass`
depends on `neotales-config` at that version. Cargo resolves a dependency from
the registry while it prepares a package for upload, so the provider cannot be
packaged until the core is on crates.io. The release used to learn that only
*after* uploading the core, which leaves the group half-released: a crates.io
version is immutable, so the provider could never be published at that version
and the tag was spent.

`gate` stages the exact artifacts a release would upload in a local Cargo
registry and packages every member against it, in dependency order, before
anything is uploaded. The staged registry is Cargo's own `local-registry` source
kind: a directory of `.crate` files and an index, not a stub that pretends to be
a registry. Dependencies come from the committed `Cargo.lock` and are checked
against its checksums; the participating members come from `cargo package`, the
same command `cargo publish` runs. The verification is offline, with crates.io
replaced by the staging directory, so a failure here is a packaging failure and
never a network failure.

`cargo publish` does not upload `target/package/*.crate`; it repackages into its
own `--target-dir`, uploads that tarball, and computes the checksum of the bytes
it sends. The gate's artifact is therefore not automatically the upload's
artifact. `publish` closes that gap in two steps:

1. It runs `cargo publish --dry-run` into the same `--target-dir` it will use for
   the upload, and compares the `tmp-crate` bytes with the gate artifact. That is
   the publish path's own packaging, and it happens before the upload. If the
   bytes differ, nothing is uploaded.
2. After the upload it compares the registry's checksum with the gate artifact,
   and also compares the `tmp-crate` file cargo just uploaded when it is still
   present. A mismatch fails the run with the version-spent recovery.

For the provider, step 1 can only run after the core is on crates.io, because the
publish path resolves the core from the registry. It therefore runs after the
core upload and before the provider upload. If it fails, the provider is not
uploaded; the core is published and a re-run of the same tag skips it.

Before any upload, `preflight` queries crates.io for BOTH members and rejects a
version that is published at different bytes or that is yanked. `publish`
rechecks both immediately before each upload, and again after the byte-proof
packaging step, to narrow the window in which the registry can change. That
window cannot be eliminated: two checks and an upload are separate requests. A
lost race spends the version, and recovery is a version bump, never a retry.

Usage
-----
    stage_gate.py gate [--root DIR] [--self-test] [--keep-work]

        Package and verify every member in dependency order. Leaves the
        release artifacts in target/package/. `--self-test` additionally
        injects the failures this gate exists to catch, checks the verdicts of
        `state`, proves both packages are byte-stable, and runs the gate from
        outside the repository root. Nothing is uploaded and no registry token
        is read.

    stage_gate.py state --manifest PATH --artifact-dir DIR [--index-base URL]

        Print one word describing whether crates.io already holds the version
        in PATH at the exact bytes in the artifact: `absent`, `identical`,
        `different` or `yanked`. `yanked` means the checksum matches but the
        version is yanked, which still blocks an upload.

    stage_gate.py preflight [--root DIR] [--artifact-dir DIR] [--index-base URL]

        Query crates.io for every member and print the plan. Exit non-zero,
        before any upload, when a member is published at different bytes, is
        yanked, or the group state is inconsistent.

    stage_gate.py publish --manifest PATH [--root DIR] [--artifact-dir DIR]
                          [--index-base URL] [--timeout SECONDS] [--keep-work]

        Recheck both members, prove the publish path packages the gate artifact,
        upload it, and require crates.io to report the same checksum. A member
        already published at that checksum is skipped. Used by release.yml.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path

# Cargo's default sparse index. The publish job's recovery check reads the same
# index; nothing here talks to the crates.io API, authenticates, or reads a
# registry token. `cargo publish` inherits the token from its own environment.
INDEX_BASE = "https://index.crates.io/"

# Package directories in dependency order. The gate packages each one after the
# previous ones are in the staged registry, so this list is also the release
# order: `config/` is `neotales-config`, `keepass/` is
# `neotales-config-keepass`. A new member is added here in the same commit that
# adds it to the workspace, exactly like the map in the version job.
MEMBERS = ("config", "keepass")

WORK_PREFIX = "release-gate-"
PUBLISH_PREFIX = "publish-"

ABSENT = "absent"
IDENTICAL = "identical"
DIFFERENT = "different"
YANKED = "yanked"

# How long to wait for the sparse index to publish an accepted upload. Cargo
# itself waits up to 60 seconds before it returns, so this is additional slack
# for the index, not the upload.
DEFAULT_PUBLISH_TIMEOUT = 120


class GateError(Exception):
    """A failure that should stop the release, carrying what to do about it."""


@dataclass(frozen=True)
class MemberState:
    """What the registry says about one member's exact artifact."""

    name: str
    version: str
    state: str
    yanked: bool = False


@dataclass(frozen=True)
class ReleasePlan:
    """The uploads a release may perform, in dependency order.

    `blocking` is non-empty exactly when nothing may be uploaded. `uploads` is
    always empty in that case, so a caller cannot accidentally publish the
    members that happened to be absent while another member is mismatched.
    """

    members: tuple[MemberState, ...]
    uploads: tuple[str, ...]
    blocking: tuple[str, ...]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Package a group release against a staged registry and "
        "publish only bytes the gate verified."
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
    gate.add_argument(
        "--keep-work",
        action="store_true",
        help="keep the unique staging directory for diagnosis",
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

    preflight = subcommands.add_parser(
        "preflight", help="query both members before any upload"
    )
    add_root_argument(preflight)
    preflight.add_argument(
        "--artifact-dir",
        type=Path,
        required=True,
        help="directory holding the gate artifacts, normally target/package",
    )
    preflight.add_argument(
        "--index-base",
        default=INDEX_BASE,
        help="registry index to ask; file:// is accepted for the self-test",
    )

    publish = subcommands.add_parser(
        "publish", help="verify the upload bytes, then publish one member"
    )
    add_root_argument(publish)
    publish.add_argument(
        "--manifest",
        type=Path,
        required=True,
        help="manifest of the member to publish",
    )
    publish.add_argument(
        "--artifact-dir",
        type=Path,
        required=True,
        help="directory holding the gate artifacts, normally target/package",
    )
    publish.add_argument(
        "--index-base",
        default=INDEX_BASE,
        help="registry index to ask; file:// is accepted for the self-test",
    )
    publish.add_argument(
        "--timeout",
        type=int,
        default=DEFAULT_PUBLISH_TIMEOUT,
        help="seconds to wait for the index to confirm an accepted upload",
    )
    publish.add_argument(
        "--keep-work",
        action="store_true",
        help="keep the unique publish directory for diagnosis",
    )

    arguments = parser.parse_args(argv)
    index_base = arguments.index_base.rstrip("/") + "/"

    try:
        if arguments.command == "gate":
            gate_release(
                arguments.root.resolve(),
                index_base,
                self_test=arguments.self_test,
                keep_work=arguments.keep_work,
            )
        elif arguments.command == "state":
            print(
                artifact_state(
                    arguments.manifest,
                    arguments.artifact_dir,
                    index_base,
                )
            )
        elif arguments.command == "preflight":
            preflight_release(
                arguments.root.resolve(),
                resolve_from(arguments.root.resolve(), arguments.artifact_dir),
                index_base,
            )
        else:
            publish_release(
                arguments.root.resolve(),
                resolve_from(arguments.root.resolve(), arguments.manifest),
                resolve_from(arguments.root.resolve(), arguments.artifact_dir),
                index_base,
                timeout=arguments.timeout,
                keep_work=arguments.keep_work,
            )
    except GateError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


def add_root_argument(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="repository root, default: the checkout this script is in",
    )


# --------------------------------------------------------------------------
# gate
# --------------------------------------------------------------------------


def gate_release(
    root: Path, index_base: str, *, self_test: bool, keep_work: bool
) -> None:
    require_repository(root)
    target = target_dir(root)
    ensure_directory(target, start=root, description="Cargo target directory")
    work = owned_temp_dir(
        target,
        WORK_PREFIX,
        start=root,
        description="release gate work directory",
    )
    print(f"staging registry in {work}")

    try:
        # The lockfile names the graph the release tests ran against. Fetch first
        # so a cold runner has the archives; a warm one is a no-op.
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
                    f"cargo package did not leave {artifact}; the artifact the "
                    "upload would use cannot be inspected"
                )
            digest = sha256(artifact)
            stage_artifact(
                registry,
                registry / "index",
                work / "unpack",
                artifact,
                name,
                version,
            )
            print(f"   staged {artifact.name} sha256 {digest}")
            artifacts.append((name, version, artifact, digest))

        print(
            "\ngate passed: every member packages and verifies against the "
            "candidate artifacts. Nothing was uploaded and no registry token "
            "was read."
        )

        if self_test:
            self_check(root, work, artifacts, index_base)
    except Exception:
        print(f"gate work kept at {work}", file=sys.stderr)
        raise
    else:
        if keep_work:
            print(f"gate work kept at {work}")
        else:
            remove_owned_quietly(work, target)


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

    unpack = unpack_root / f"{name}-{version}"
    if unpack.exists() or unpack.is_symlink():
        raise GateError(
            f"refusing to reuse the unpack directory {unpack}; a gate run must "
            "own the directory it extracts into"
        )
    unpack.mkdir(parents=True)
    with tarfile.open(artifact, "r:gz") as archive:
        extract_archive(archive, unpack)

    manifest = unpack / f"{name}-{version}" / "Cargo.toml"
    entry = index_entry(
        name=name,
        version=version,
        manifest=tomllib.loads(manifest.read_text(encoding="utf-8")),
        checksum=sha256(artifact),
    )
    append_index(index_root, name, json.dumps(entry, separators=(",", ":")))


def extract_archive(archive: tarfile.TarFile, destination: Path) -> None:
    """Extract a crate archive without allowing it outside `destination`."""
    root = destination.resolve()
    for member in archive.getmembers():
        if member.issym() or member.islnk():
            raise GateError(
                f"{member.name} is a link; refusing to extract it from a crate archive"
            )
        resolved = (destination / member.name).resolve()
        if not resolved.is_relative_to(root):
            raise GateError(
                f"{member.name} escapes {destination}; refusing to extract it"
            )
    if hasattr(tarfile, "data_filter"):
        archive.extractall(destination, filter="data")
        return
    # Python older than 3.12 has no data filter. The checks above already refuse
    # links and escaping names, which is the part a crate archive needs.
    archive.extractall(destination)


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


def classify_artifact(
    manifest: Path,
    artifact_dir: Path,
    index_base: str,
    *,
    quiet: bool = False,
) -> MemberState:
    """Classify the artifact against the registry without uploading anything."""
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
        if not quiet:
            print(
                f"{name} {version} is not published; {artifact.name} sha256 {local} "
                "would be a new upload",
                file=sys.stderr,
            )
        return MemberState(name, version, ABSENT)

    published_entry = json.loads(line)
    published = published_entry.get("cksum")
    yanked = bool(published_entry.get("yanked", False))
    if published == local:
        if yanked:
            if not quiet:
                print(
                    f"crates.io holds {name} {version} at sha256 {local}, the exact "
                    f"artifact in {artifact.name}, but the version is yanked. A "
                    "yanked version is not a completed release and cannot be "
                    "uploaded again.",
                    file=sys.stderr,
                )
            return MemberState(name, version, YANKED, yanked=True)
        if not quiet:
            print(
                f"crates.io already holds {name} {version} at sha256 {local}, the "
                f"exact artifact in {artifact.name}",
                file=sys.stderr,
            )
        return MemberState(name, version, IDENTICAL)

    if not quiet:
        suffix = " The published version is also yanked." if yanked else ""
        print(
            f"crates.io holds {name} {version} at sha256 "
            f"{published or '<no checksum>'}, but {artifact.name} hashes to "
            f"{local}.{suffix}",
            file=sys.stderr,
        )
    return MemberState(name, version, DIFFERENT, yanked=yanked)


def artifact_state(manifest: Path, artifact_dir: Path, index_base: str) -> str:
    return classify_artifact(manifest, artifact_dir, index_base).state


def preflight_release(
    root: Path, artifact_dir: Path, index_base: str
) -> ReleasePlan:
    """Query both members and fail before any upload if the group cannot go."""
    require_repository(root)
    members = [
        classify_artifact(
            root / directory / "Cargo.toml",
            artifact_dir,
            index_base,
            quiet=True,
        )
        for directory in MEMBERS
    ]
    plan = decide(members)
    for member in plan.members:
        detail = {
            ABSENT: "would upload",
            IDENTICAL: "already published at the same checksum",
            YANKED: "published at the same checksum but yanked",
            DIFFERENT: "published at different bytes",
        }.get(member.state, "unknown")
        print(f"{member.name} {member.version}: {member.state} ({detail})")

    if plan.blocking:
        lines = ["release blocked before any upload:"]
        lines.extend(f"  - {reason}" for reason in plan.blocking)
        lines.append(
            "Nothing was uploaded. Fix the crates.io state or bump the version in "
            "every member manifest, then run again."
        )
        raise GateError("\n".join(lines))

    if plan.uploads:
        print("plan: upload " + ", ".join(plan.uploads))
    else:
        print("plan: nothing to upload")
    return plan


def decide(members: list[MemberState]) -> ReleasePlan:
    """Turn registry states into uploads, or into blocking reasons.

    This is deliberately pure: it is the function the self-test injects a
    mismatched or yanked provider into, and it must never report an upload when
    any member blocks the release.
    """
    blocking: list[str] = []
    uploads: list[str] = []

    for member in members:
        if member.state == ABSENT:
            uploads.append(member.name)
        elif member.state == IDENTICAL:
            continue
        elif member.state == YANKED:
            blocking.append(
                f"{member.name} {member.version} is published at the artifact's "
                "checksum but is yanked. A yanked version is not installable and "
                "cannot be uploaded again. If the yank was ours, unyank it on "
                "crates.io and re-run; otherwise bump the version."
            )
        elif member.state == DIFFERENT:
            suffix = " It is also yanked." if member.yanked else ""
            blocking.append(
                f"{member.name} {member.version} is already published at different "
                "bytes. A crates.io version cannot be replaced: bump the version in "
                f"every member manifest and release a new tag.{suffix}"
            )
        else:
            blocking.append(
                f"{member.name} {member.version} has the unexpected registry state "
                f"{member.state!r}."
            )

    # A later member cannot exist while an earlier one does not: the provider
    # requires the core at the group version. Treat that as an inconsistent
    # registry, not as a green light to upload the core around it.
    absent: str | None = None
    for member in members:
        if member.state == ABSENT:
            absent = member.name if absent is None else absent
        elif absent is not None:
            blocking.append(
                f"{member.name} {member.version} is published while {absent} is not. "
                "The group state is inconsistent; nothing may be uploaded until "
                "the registry agrees on the group."
            )

    if blocking:
        return ReleasePlan(tuple(members), (), tuple(blocking))
    return ReleasePlan(tuple(members), tuple(uploads), ())


def execute_plan(plan: ReleasePlan, publish) -> None:
    """Publish a plan's uploads in order, or refuse when it blocks."""
    if plan.blocking:
        raise GateError("\n".join(plan.blocking))
    for name in plan.uploads:
        publish(name)


# --------------------------------------------------------------------------
# publish: prove the upload bytes, then upload one member
# --------------------------------------------------------------------------


def publish_release(
    root: Path,
    manifest: Path,
    artifact_dir: Path,
    index_base: str,
    *,
    timeout: int,
    keep_work: bool,
) -> None:
    require_repository(root)
    name, version = manifest_identity(manifest)
    directory = member_directory(root, name)
    artifact = artifact_dir / f"{name}-{version}.crate"
    if not artifact.is_file():
        raise GateError(
            f"{artifact} is missing. Run `stage_gate.py gate` first so the upload "
            "can be compared with the artifact the gate verified."
        )
    expected = sha256(artifact)

    # Recheck the whole group immediately before this side effect. The separate
    # preflight step runs before the token exchange; this is the race guard.
    plan = preflight_release(root, artifact_dir, index_base)
    member = member_state(plan, name)
    if member.state == IDENTICAL:
        print(
            f"{name} {version} is already published at sha256 {expected}; "
            "nothing to upload."
        )
        return
    if member.state != ABSENT:
        raise GateError(
            f"{name} {version} is {member.state}; refusing to upload it."
        )
    enforce_upload_order(root, plan, directory)

    target = target_dir(root)
    ensure_directory(target, start=root, description="Cargo target directory")
    publish_dir = owned_temp_dir(
        target,
        f"{PUBLISH_PREFIX}{name}-",
        start=root,
        description="publish work directory",
    )
    try:
        # `cargo publish` repackages into its own target directory and uploads
        # that tarball. A dry run through the same path leaves exactly the
        # artifact the real publish will upload, so it can be compared with the
        # gate artifact before anything is sent.
        run(
            [
                "cargo",
                "publish",
                "--package",
                name,
                "--locked",
                "--dry-run",
                "--target-dir",
                str(publish_dir),
            ],
            root,
        )
        dry_artifact = publish_tarball(publish_dir, name, version)
        if not dry_artifact.is_file():
            raise GateError(
                f"cargo publish --dry-run did not leave {dry_artifact}; the upload "
                "bytes cannot be compared with the gate artifact"
            )
        dry_digest = sha256(dry_artifact)
        if dry_digest != expected:
            raise GateError(
                f"the publish path packaged {name} {version} as sha256 {dry_digest}, "
                f"but the gate verified {expected}. Nothing was uploaded: the gate's "
                "proof does not cover these bytes. Fix the packaging "
                "nondeterminism or bump the version, then tag again. Do not publish "
                "by hand."
            )
        print(
            f"publish path produced the gate artifact: {name} {version} "
            f"sha256 {expected}"
        )

        # Recheck again after the dry-run, which takes time and talks to the
        # registry. Another run may have published the same artifact, the version
        # may have been yanked, or a different artifact may have taken it.
        plan = preflight_release(root, artifact_dir, index_base)
        member = member_state(plan, name)
        if member.state == IDENTICAL:
            print(
                f"{name} {version} was published by another run at the exact "
                "checksum during the dry run; nothing to upload."
            )
            return
        if member.state != ABSENT:
            raise GateError(
                f"{name} {version} became {member.state} during the dry run; "
                "refusing to upload it."
            )
        enforce_upload_order(root, plan, directory)

        command = [
            "cargo",
            "publish",
            "--package",
            name,
            "--locked",
            "--target-dir",
            str(publish_dir),
        ]
        print("   $ " + shlex.join(command), flush=True)
        result = subprocess.run(command, check=False, cwd=root)

        # Cargo can return a failure after the registry accepted the upload (its
        # own confirmation wait can time out). The index is the authority.
        state = wait_for_published(
            manifest,
            artifact_dir,
            index_base,
            timeout=timeout,
        )
        if state == ABSENT:
            if result.returncode == 0:
                raise GateError(
                    f"cargo publish succeeded, but crates.io did not report {name} "
                    f"{version} within {timeout} seconds. The upload may be queued. "
                    "Do not publish by hand: re-run the job; preflight will report "
                    "absent (retry) or identical (skip)."
                )
            raise GateError(
                f"cargo publish exited {result.returncode} and crates.io does not "
                f"report {name} {version}. Check the cargo error above and the "
                "token; nothing is known to have been accepted. Do not publish by "
                "hand."
            )
        if state == YANKED:
            raise GateError(
                f"crates.io reports {name} {version} at the artifact's checksum but "
                "yanked. The version is spent and cannot be uploaded again. Unyank "
                "it on crates.io if that was our yank, or bump the version."
            )
        if state == DIFFERENT:
            raise GateError(
                f"crates.io reports {name} {version} at different bytes from the "
                f"gate artifact sha256 {expected}. The version is spent; bump the "
                "patch version in every member manifest and release a new tag. Do "
                "not retry this version."
            )
        if state != IDENTICAL:
            raise GateError(
                f"crates.io reports the unexpected state {state!r} for {name} "
                f"{version}; refusing to report the release as complete."
            )

        if result.returncode != 0:
            print(
                f"cargo publish exited {result.returncode}, but crates.io holds "
                f"{name} {version} at the exact gate artifact sha256 {expected}. "
                "The upload was accepted; treat the cargo failure as the cause to "
                "investigate, not as a reason to retry the upload.",
                file=sys.stderr,
            )

        uploaded = publish_tarball(publish_dir, name, version)
        if uploaded.is_file():
            uploaded_digest = sha256(uploaded)
            if uploaded_digest != expected:
                print(
                    f"warning: the local publish tarball now hashes to "
                    f"{uploaded_digest}, not the gate artifact {expected}. The "
                    "registry checksum is authoritative and already matched; the "
                    "local file may have been rewritten after the upload.",
                    file=sys.stderr,
                )
            else:
                print(
                    f"the publish tarball cargo uploaded hashes to {expected}, the "
                    "gate artifact"
                )
        else:
            print(
                "cargo removed its local publish tarball; the registry checksum is "
                "the evidence for the uploaded bytes"
            )

        print(
            f"verified: crates.io holds {name} {version} at sha256 {expected}, the "
            "gate artifact"
        )
    except Exception:
        print(f"publish work kept at {publish_dir}", file=sys.stderr)
        raise
    else:
        if keep_work:
            print(f"publish work kept at {publish_dir}")
        else:
            remove_owned_quietly(publish_dir, target)


def member_state(plan: ReleasePlan, name: str) -> MemberState:
    for member in plan.members:
        if member.name == name:
            return member
    raise GateError(f"{name} is not one of the release members")


def enforce_upload_order(root: Path, plan: ReleasePlan, directory: str) -> None:
    """A member may upload only after every earlier member is already there."""
    index = MEMBERS.index(directory)
    for earlier in MEMBERS[:index]:
        name, _version = manifest_identity(root / earlier / "Cargo.toml")
        state = member_state(plan, name).state
        if state == IDENTICAL:
            continue
        raise GateError(
            f"{name} is {state}, not visible as already published. Publish the "
            f"members in dependency order: {', '.join(MEMBERS)}. If this is a "
            "re-run, the index may be lagging; re-run the job rather than "
            "publishing out of order."
        )


def wait_for_published(
    manifest: Path,
    artifact_dir: Path,
    index_base: str,
    *,
    timeout: int,
) -> str:
    """Wait for the index to show a non-absent state, or return absent."""
    deadline = time.monotonic() + timeout
    while True:
        state = classify_artifact(
            manifest, artifact_dir, index_base, quiet=True
        ).state
        if state != ABSENT:
            return state
        if time.monotonic() >= deadline:
            return ABSENT
        time.sleep(3)


def publish_tarball(publish_dir: Path, name: str, version: str) -> Path:
    return publish_dir / "package" / "tmp-crate" / f"{name}-{version}.crate"


# --------------------------------------------------------------------------
# self-test: the failures this gate exists to catch
# --------------------------------------------------------------------------


def self_check(
    root: Path,
    work: Path,
    artifacts: list[tuple[str, str, Path, str]],
    index_base: str,
) -> None:
    """Inject the failures the gate must catch, and the verdicts `state` returns.

    These are the release's own failure paths, so they are exercised on every CI
    run rather than trusted. Nothing is uploaded and no token is read.
    """
    core_name, core_version, core_artifact, core_digest = artifacts[0]
    last_name, last_version, _last_artifact, last_digest = artifacts[-1]

    print("\n== self-test: failure injection")

    outside = Path(tempfile.mkdtemp(prefix="stage-gate-outside-"))
    previous = Path.cwd()
    try:
        os.chdir(outside)
        try:
            # 1. The pre-fix blocker: the core is not in the registry at all.
            missing = copy_registry(
                work / "registry", work / "inject-missing-core"
            )
            (missing / core_artifact.name).unlink()
            remove_index(missing / "index", core_name)

            output = expect_failure(
                package_command(work, last_name, missing),
                cwd=root,
                description=f"packaging {last_name} without {core_name} in the registry",
            )
            assert_message(
                output,
                core_name,
                description="the error names the missing dependency",
            )
            assert_not_message(
                output,
                "could not find",
                description="the failure injection ran in the repository root",
            )
            assert_not_message(
                output,
                str(outside),
                description="the failure injection did not run in the caller's cwd",
            )

            # 2. A version the provider does not require: the group versions drifted.
            drifted_version = bump_major(core_version)
            drifted = copy_registry(
                work / "registry", work / "inject-version-drift"
            )
            rename_index_version(
                drifted / "index", core_name, core_version, drifted_version
            )
            (drifted / core_artifact.name).rename(
                drifted / f"{core_name}-{drifted_version}.crate"
            )

            output = expect_failure(
                package_command(work, last_name, drifted),
                cwd=root,
                description=(
                    f"packaging {last_name} against {core_name} {drifted_version}"
                ),
            )
            assert_message(
                output,
                core_name,
                description="the error names the requirement it could not satisfy",
            )
        finally:
            os.chdir(previous)
    finally:
        remove_owned(outside, outside.parent)
    print(
        "   ok: failure injection ran from outside the repository root and still "
        "used the workspace at the requested root"
    )

    # 3. The state verdicts and the upload plan, against a local index built from
    #    the staged entries, so the comparison path is tested without publishing.
    index = work / "inject-state-index"
    index.mkdir()
    original: dict[str, str] = {}
    for directory in MEMBERS:
        name, version = manifest_identity(root / directory / "Cargo.toml")
        staged_line = index_line(
            (work / "registry" / "index" / index_path(name)).read_text(
                encoding="utf-8"
            ),
            name,
            version,
        )
        if staged_line is None:
            raise GateError(f"self-test: the staged index has no entry for {name}")
        original[name] = staged_line
        append_index(index, name, staged_line)

    base = index.resolve().as_uri().rstrip("/") + "/"
    artifact_dir = target_dir(root) / "package"
    manifests = {
        name: root / member_directory(root, name) / "Cargo.toml"
        for name in (core_name, last_name)
    }

    def state_of(name: str) -> str:
        return classify_artifact(manifests[name], artifact_dir, base, quiet=True).state

    def plan_now() -> ReleasePlan:
        return decide(
            [
                MemberState(name, version, state_of(name))
                for name, version in (
                    (core_name, core_version),
                    (last_name, last_version),
                )
            ]
        )

    expect_state(
        IDENTICAL,
        state_of(core_name),
        description="an artifact that matches the published checksum",
    )
    expect_state(
        IDENTICAL,
        state_of(last_name),
        description="the provider matching its published checksum",
    )
    expect_plan(
        plan_now(),
        uploads=(),
        description="both members already published at the gate checksums",
    )

    rewrite_entry(index, last_name, cksum="0" * 64)
    expect_state(
        DIFFERENT,
        state_of(last_name),
        description="a provider whose bytes differ from the published checksum",
    )
    expect_blocked(
        plan_now(),
        "a provider published at different bytes",
    )

    rewrite_entry(index, last_name, cksum=last_digest, yanked=True)
    expect_state(
        YANKED,
        state_of(last_name),
        description="a provider matching but yanked",
    )
    expect_blocked(
        plan_now(),
        "a provider matching but yanked",
    )

    mirror_state_index(index, original, core_name, last_name)
    rewrite_entry(index, core_name, cksum="0" * 64)
    expect_state(
        DIFFERENT,
        state_of(core_name),
        description="a core whose bytes differ from the published checksum",
    )
    expect_blocked(
        plan_now(),
        "a core published at different bytes",
    )

    rewrite_entry(index, core_name, cksum=core_digest, yanked=True)
    expect_state(
        YANKED,
        state_of(core_name),
        description="a core matching but yanked",
    )
    expect_blocked(
        plan_now(),
        "a core matching but yanked",
    )

    mirror_state_index(index, original, core_name, last_name)
    remove_index(index, last_name)
    expect_state(
        ABSENT,
        state_of(last_name),
        description="a provider version the index does not list",
    )
    expect_plan(
        plan_now(),
        uploads=(last_name,),
        description="recovery: the core is published and the provider is absent",
    )

    remove_index(index, core_name)
    expect_state(
        ABSENT,
        state_of(core_name),
        description="a core version the index does not list",
    )
    expect_plan(
        plan_now(),
        uploads=(core_name, last_name),
        description="a fresh group with neither member published",
    )

    append_index(index, last_name, original[last_name])
    expect_blocked(
        plan_now(),
        "a provider published while the core is absent",
    )

    append_index(index, core_name, original[core_name])
    expect_plan(
        plan_now(),
        uploads=(),
        description="both members restored at the gate checksums",
    )

    # 4. The concrete injection: provider different or yanked => the plan has no
    #    uploads, and both CLIs refuse from outside the repository root.
    cli_outside = Path(tempfile.mkdtemp(prefix="stage-gate-cli-outside-"))
    try:
        for injected, needle, description in (
            ({"cksum": "0" * 64, "yanked": False}, "different", "different bytes"),
            ({"cksum": last_digest, "yanked": True}, "yanked", "matching but yanked"),
        ):
            mirror_state_index(index, original, core_name, last_name)
            rewrite_entry(index, last_name, **injected)
            plan = plan_now()
            expect_blocked(plan, f"provider {description}")
            calls: list[str] = []
            try:
                execute_plan(plan, calls.append)
            except GateError:
                pass
            if calls:
                raise GateError(
                    f"self-test: provider {description} caused uploads {calls}; a "
                    "blocking state must upload nothing"
                )
            print(f"   ok: provider {description} produces zero uploads")
            preflight_cli_fails(
                root,
                artifact_dir,
                base,
                cli_outside,
                needle,
                description=f"provider {description}",
            )
            publish_cli_fails(
                root,
                manifests[last_name],
                artifact_dir,
                base,
                cli_outside,
                needle,
                description=f"provider {description}",
            )
    finally:
        remove_owned(cli_outside, cli_outside.parent)

    mirror_state_index(index, original, core_name, last_name)

    # 5. The gate artifact is byte-stable for BOTH packages, and the core's
    #    publish path packages the same bytes the gate verified.
    verify_package_stability(root, work, artifacts)
    verify_publish_path_core(root, work, artifacts[0])
    verify_upload_comparator(work, artifacts)
    verify_publish_guard(root, work, artifacts[0], artifacts[-1])

    # 6. Unsafe cleanup guards.
    test_cleanup_guards()

    # 7. The full gate from outside the repository root. This is a subprocess, so
    #    it also exercises the default --root and every path argument from a cwd
    #    that is not the checkout.
    verify_gate_from_outside(root, artifacts, index_base)

    print(
        "self-test passed: the gate rejects a missing or drifted core; both "
        "members are byte-stable; mismatched and matching-but-yanked states block "
        "with zero uploads; the publish path and recovery verdicts hold."
    )


def mirror_state_index(
    index: Path,
    original: dict[str, str],
    core_name: str,
    last_name: str,
) -> None:
    """Reset the injected index to the two original staged entries."""
    for name in (core_name, last_name):
        path = index / index_path(name)
        path.write_text(original[name] + "\n", encoding="utf-8")


def preflight_cli_fails(
    root: Path,
    artifact_dir: Path,
    base: str,
    cwd: Path,
    needle: str,
    *,
    description: str,
) -> None:
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "preflight",
        "--root",
        str(root),
        "--artifact-dir",
        str(artifact_dir),
        "--index-base",
        base,
    ]
    result = subprocess.run(
        command,
        check=False,
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    output = result.stdout + result.stderr
    if result.returncode == 0:
        raise GateError(
            f"self-test: preflight accepted {description}, but it must block"
        )
    assert_message(output, needle, description=f"preflight names {description}")
    assert_message(
        output, "Nothing was uploaded", description="preflight says nothing was sent"
    )
    print(f"   ok: preflight refuses {description} from outside the repository")


def publish_cli_fails(
    root: Path,
    manifest: Path,
    artifact_dir: Path,
    base: str,
    cwd: Path,
    needle: str,
    *,
    description: str,
) -> None:
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "publish",
        "--root",
        str(root),
        "--manifest",
        str(manifest),
        "--artifact-dir",
        str(artifact_dir),
        "--index-base",
        base,
    ]
    environment = dict(os.environ)
    # The refusal must happen before cargo, so no token is needed. Remove any
    # inherited token rather than read it.
    environment.pop("CARGO_REGISTRY_TOKEN", None)
    result = subprocess.run(
        command,
        check=False,
        cwd=cwd,
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=environment,
    )
    output = result.stdout + result.stderr
    if result.returncode == 0:
        raise GateError(
            f"self-test: publish accepted {description}, but it must block"
        )
    assert_message(output, needle, description=f"publish names {description}")
    assert_not_message(
        output,
        "Uploading",
        description=f"publish uploaded nothing for {description}",
    )
    assert_not_message(
        output,
        "Packaging",
        description=f"publish never reached cargo packaging for {description}",
    )
    print(f"   ok: publish refuses {description} before cargo runs")


def verify_package_stability(
    root: Path,
    work: Path,
    artifacts: list[tuple[str, str, Path, str]],
) -> None:
    config = work / "cargo-config.toml"
    for name, version, artifact, digest in artifacts:
        target = owned_temp_dir(
            work,
            f"stability-{name}-",
            start=work,
            description="package stability directory",
        )
        try:
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
                    "--target-dir",
                    str(target),
                ],
                root,
            )
            produced = target / "package" / f"{name}-{version}.crate"
            if not produced.is_file():
                raise GateError(
                    f"self-test: cargo package did not reproduce {artifact.name}"
                )
            reproduced = sha256(produced)
            if reproduced != digest:
                raise GateError(
                    f"self-test: {name} packaged as sha256 {reproduced} on the "
                    f"second run, not {digest}; the gate artifact is not stable"
                )
            print(f"   ok: cargo package reproduced {name} at sha256 {digest}")
        finally:
            remove_owned_quietly(target, work)


def verify_publish_path_core(
    root: Path,
    work: Path,
    core: tuple[str, str, Path, str],
) -> None:
    name, version, _artifact, digest = core
    target = owned_temp_dir(
        work,
        "publish-path-",
        start=work,
        description="publish path directory",
    )
    try:
        run(
            [
                "cargo",
                "publish",
                "--package",
                name,
                "--locked",
                "--dry-run",
                "--target-dir",
                str(target),
            ],
            root,
        )
        produced = publish_tarball(target, name, version)
        if not produced.is_file():
            raise GateError(
                "self-test: cargo publish --dry-run did not leave its tarball at "
                f"{produced}"
            )
        reproduced = sha256(produced)
        if reproduced != digest:
            raise GateError(
                f"self-test: the publish path packaged {name} as sha256 "
                f"{reproduced}, not the gate artifact {digest}"
            )
        print(
            f"   ok: the core publish path produced the gate artifact at "
            f"sha256 {digest}"
        )
    finally:
        remove_owned_quietly(target, work)


def verify_upload_comparator(
    work: Path,
    artifacts: list[tuple[str, str, Path, str]],
) -> None:
    for name, version, artifact, digest in artifacts:
        directory = work / f"upload-artifact-{name}" / "package" / "tmp-crate"
        directory.mkdir(parents=True)
        captured = directory / f"{name}-{version}.crate"
        shutil.copy2(artifact, captured)
        assert_same_bytes(
            expected=digest,
            actual=captured,
            description=f"the captured upload bytes for {name}",
        )
        with captured.open("ab") as handle:
            handle.write(b"mutated")
        expect_byte_failure(
            expected=digest,
            actual=captured,
            description=f"a mutated upload capture for {name}",
        )
        print(f"   ok: upload-byte comparison covers {name} in both directions")


def assert_same_bytes(*, expected: str, actual: Path, description: str) -> None:
    if not actual.is_file():
        raise GateError(f"self-test: {description}: {actual} is missing")
    digest = sha256(actual)
    if digest != expected:
        raise GateError(
            f"self-test: {description} hashes to {digest}, expected {expected}"
        )


def expect_byte_failure(*, expected: str, actual: Path, description: str) -> None:
    try:
        assert_same_bytes(expected=expected, actual=actual, description=description)
    except GateError:
        print(f"   ok: upload-byte comparison rejects {description}")
        return
    raise GateError(f"self-test: upload-byte comparison accepted {description}")


def verify_publish_guard(
    root: Path,
    work: Path,
    core: tuple[str, str, Path, str],
    provider: tuple[str, str, Path, str],
) -> None:
    """A gate artifact that differs from the publish path must abort early.

    The provider cannot be exercised this way before the core is published: its
    publish path resolves the core from crates.io. The same byte comparison
    function is used for both, and it is tested for both packages above. This
    test drives the core through the real command so the early abort is observed
    end to end, not just in the comparator.
    """
    name, version, artifact, _digest = core
    guard_index = work / "guard-index"
    guard_index.mkdir()
    base = guard_index.resolve().as_uri().rstrip("/") + "/"

    artifacts = work / "guard-artifacts"
    artifacts.mkdir()
    fake = artifacts / f"{name}-{version}.crate"
    shutil.copy2(artifact, fake)
    with fake.open("ab") as handle:
        handle.write(b"mutated")
    shutil.copy2(provider[2], artifacts / provider[2].name)

    outside = Path(tempfile.mkdtemp(prefix="stage-gate-guard-outside-"))
    try:
        command = [
            sys.executable,
            str(Path(__file__).resolve()),
            "publish",
            "--root",
            str(root),
            "--manifest",
            str(root / member_directory(root, name) / "Cargo.toml"),
            "--artifact-dir",
            str(artifacts),
            "--index-base",
            base,
        ]
        environment = dict(os.environ)
        environment.pop("CARGO_REGISTRY_TOKEN", None)
        result = subprocess.run(
            command,
            check=False,
            cwd=outside,
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=environment,
        )
        output = result.stdout + result.stderr
        if result.returncode == 0:
            raise GateError(
                "self-test: publish accepted a gate artifact that differs from "
                "the publish path"
            )
        assert_message(
            output,
            "publish path packaged",
            description="the exact-byte guard names the publish path's artifact",
        )
        assert_message(
            output,
            "Nothing was uploaded",
            description="the exact-byte guard says nothing was uploaded",
        )
        assert_not_message(
            output,
            "Uploaded",
            description="the exact-byte guard did not upload",
        )
        leftover = publish_work_from_output(output)
        if leftover is not None:
            remove_owned(leftover, target_dir(root))
        print(
            "   ok: a gate artifact that differs from the publish path refuses "
            "before the upload"
        )
    finally:
        remove_owned(outside, outside.parent)


def publish_work_from_output(output: str) -> Path | None:
    marker = "publish work kept at "
    for line in output.splitlines():
        if line.startswith(marker):
            return Path(line[len(marker) :].strip())
    return None


def test_cleanup_guards() -> None:
    sandbox = Path(tempfile.mkdtemp(prefix="stage-gate-symlink-"))
    try:
        real = sandbox / "real-target"
        real.mkdir()
        (real / "sentinel").write_text("keep", encoding="utf-8")
        repository = sandbox / "repo"
        repository.mkdir()
        (repository / "target").symlink_to(real, target_is_directory=True)

        try:
            owned_temp_dir(
                repository / "target",
                WORK_PREFIX,
                start=repository,
                description="release gate work directory",
            )
        except GateError as error:
            if "symbolic link" not in str(error):
                raise
            print("   ok: a symlinked Cargo target directory is refused")
        else:
            raise GateError(
                "self-test: a symlinked target directory was accepted; the gate "
                "could delete a directory outside the repository"
            )

        if not (real / "sentinel").is_file():
            raise GateError(
                "self-test: the symlink guard deleted the target's contents"
            )
        if list(real.glob(f"{WORK_PREFIX}*")):
            raise GateError(
                "self-test: the symlink guard created a work directory in the "
                "target of the symlink"
            )

        outside_file = sandbox / "outside.txt"
        outside_file.write_text("keep", encoding="utf-8")
        owned = sandbox / "owned"
        owned.mkdir()
        try:
            remove_owned(outside_file, owned)
        except GateError:
            print("   ok: cleanup refuses a path outside its owned root")
        else:
            raise GateError(
                "self-test: cleanup removed a path outside its owned root"
            )
        if not outside_file.is_file():
            raise GateError("self-test: cleanup deleted an unowned file")
    finally:
        remove_owned(sandbox, sandbox.parent)


def verify_gate_from_outside(
    root: Path,
    artifacts: list[tuple[str, str, Path, str]],
    index_base: str,
) -> None:
    outside = Path(tempfile.mkdtemp(prefix="stage-gate-outside-gate-"))
    try:
        command = [
            sys.executable,
            str(Path(__file__).resolve()),
            "gate",
            "--root",
            str(root),
            "--index-base",
            index_base,
        ]
        result = subprocess.run(
            command,
            check=False,
            cwd=outside,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
        output = result.stdout + result.stderr
        if result.returncode != 0:
            raise GateError(
                "self-test: the gate failed when run from outside the repository "
                f"root:\n{output}"
            )
        assert_message(
            output,
            "gate passed",
            description="the gate run from outside the repository root",
        )
        for name, _version, artifact, digest in artifacts:
            if not artifact.is_file():
                raise GateError(
                    f"self-test: the outside-root gate lost {artifact.name}"
                )
            reproduced = sha256(artifact)
            if reproduced != digest:
                raise GateError(
                    f"self-test: the outside-root gate repackaged {name} as "
                    f"sha256 {reproduced}, not {digest}"
                )
        print(
            "   ok: the gate ran from outside the repository root and reproduced "
            "both artifacts"
        )
    finally:
        remove_owned(outside, outside.parent)


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


def expect_failure(command: list[str], *, cwd: Path, description: str) -> str:
    result = subprocess.run(
        command,
        check=False,
        cwd=cwd,
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


def assert_not_message(output: str, needle: str, *, description: str) -> None:
    if needle in output:
        raise GateError(
            f"self-test: {description} did not hold; did not expect {needle!r} in:\n"
            f"{output}"
        )
    print(f"   ok: {description}")


def expect_state(expected: str, actual: str, *, description: str) -> None:
    if expected != actual:
        raise GateError(
            f"self-test: {description} reported {actual!r}, expected {expected!r}"
        )
    print(f"   ok: {description} reports {expected}")


def expect_plan(
    plan: ReleasePlan,
    *,
    uploads: tuple[str, ...],
    description: str,
) -> None:
    if plan.blocking:
        raise GateError(
            f"self-test: {description} was blocked: {'; '.join(plan.blocking)}"
        )
    if plan.uploads != uploads:
        raise GateError(
            f"self-test: {description} planned {plan.uploads}, expected {uploads}"
        )
    print(f"   ok: {description} plans {uploads or 'no uploads'}")


def expect_blocked(plan: ReleasePlan, description: str) -> None:
    if not plan.blocking:
        raise GateError(f"self-test: {description} was not blocked")
    if plan.uploads:
        raise GateError(
            f"self-test: {description} was blocked but still planned uploads "
            f"{plan.uploads}"
        )
    print(f"   ok: {description} is blocked with zero uploads")


def copy_registry(source: Path, destination: Path) -> Path:
    if destination.exists() or destination.is_symlink():
        raise GateError(
            f"self-test: refusing to reuse {destination}; each injected registry "
            "must be a directory this run created"
        )
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


def rewrite_entry(index_root: Path, name: str, **changes) -> None:
    path = index_root / index_path(name)
    lines = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        entry = json.loads(line)
        if entry.get("name") == name:
            entry.update(changes)
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
# safe filesystem ownership
# --------------------------------------------------------------------------


def require_repository(root: Path) -> None:
    if not (root / "Cargo.toml").is_file():
        raise GateError(
            f"{root} does not look like the repository root: no Cargo.toml there. "
            "Pass --root with the checkout that carries the workspace manifest."
        )
    if not (root / "Cargo.lock").is_file():
        raise GateError(
            f"{root} has no Cargo.lock. The gate stages the locked graph, so the "
            "release must be cut from the committed lockfile."
        )


def ensure_directory(path: Path, *, start: Path, description: str) -> None:
    refuse_symlink_ancestors(path, start=start, description=description)
    if path.exists() and not path.is_dir():
        raise GateError(
            f"{description} {path} exists and is not a directory. The release "
            "gate will not replace it; move it aside or point CARGO_TARGET_DIR "
            "elsewhere."
        )
    path.mkdir(parents=True, exist_ok=True)
    if path.is_symlink() or not path.is_dir():
        raise GateError(
            f"{description} {path} is not a real directory. The release gate "
            "refuses to create or delete through a symbolic link; point "
            "CARGO_TARGET_DIR at a real directory."
        )
    refuse_symlink_ancestors(path, start=start, description=description)


def owned_temp_dir(
    parent: Path,
    prefix: str,
    *,
    start: Path,
    description: str,
) -> Path:
    """Create a unique, private directory under `parent` and prove containment."""
    ensure_directory(parent, start=start, description=description)
    created = Path(tempfile.mkdtemp(prefix=prefix, dir=parent))
    if created.is_symlink() or not created.is_dir():
        raise GateError(f"{description} {created} is not a real directory")
    refuse_symlink_ancestors(created, start=start, description=description)
    resolved_parent = parent.resolve()
    if not created.resolve().is_relative_to(resolved_parent):
        raise GateError(
            f"{description} {created} escaped {resolved_parent}; refusing to use it"
        )
    return created


def refuse_symlink_ancestors(
    path: Path, *, start: Path | None, description: str
) -> None:
    """Refuse a path that traverses a symbolic link below `start`.

    A fixed cleanup path such as `target/release-gate` can delete an unrelated
    directory when `target` is a symlink. The gate creates a unique owned
    directory and, in addition, refuses any symlinked component it would write
    or delete through.
    """
    if ".." in Path(path).parts:
        raise GateError(
            f"{description} {path} contains '..'. The release gate refuses a "
            "path it cannot check component by component."
        )
    absolute = Path(os.path.abspath(path))
    if start is not None:
        start_absolute = Path(os.path.abspath(start))
        try:
            relative = absolute.relative_to(start_absolute)
        except ValueError:
            relative = None
        if relative is not None:
            current = start_absolute
            for part in relative.parts:
                current = current / part
                if current.is_symlink():
                    raise GateError(
                        f"{description} {current} is a symbolic link. The release "
                        "gate refuses to create or delete through a symlinked "
                        "path; point CARGO_TARGET_DIR at a real directory."
                    )
            return

    current = Path(absolute.anchor)
    for part in absolute.parts[1:]:
        current = current / part
        if current.is_symlink():
            raise GateError(
                f"{description} {current} is a symbolic link. The release gate "
                "refuses to create or delete through a symlinked path; point "
                "CARGO_TARGET_DIR at a real directory."
            )


def remove_owned(path: Path, owned_root: Path) -> None:
    """Remove a path only when it is a real child of the directory we own."""
    if ".." in path.parts or ".." in owned_root.parts:
        raise GateError(
            f"refusing to remove {path} under {owned_root}: a path containing "
            "'..' cannot be checked component by component"
        )
    absolute = Path(os.path.abspath(path))
    root = Path(os.path.abspath(owned_root))
    if absolute == root:
        raise GateError(
            f"refusing to remove {absolute}: it is the owned root, not a child"
        )
    if not absolute.exists() and not absolute.is_symlink():
        return
    if absolute.is_symlink():
        raise GateError(
            f"refusing to remove {absolute}: it is a symbolic link, not the "
            "directory this run created"
        )
    refuse_symlink_ancestors(absolute, start=root, description="cleanup path")
    if not absolute.resolve().is_relative_to(root.resolve()):
        raise GateError(
            f"refusing to remove {absolute}: it is not under {root}, the "
            "directory this run created"
        )
    if absolute.is_dir():
        shutil.rmtree(absolute)
    else:
        absolute.unlink()


def remove_owned_quietly(path: Path, owned_root: Path) -> None:
    try:
        remove_owned(path, owned_root)
    except GateError as error:
        print(f"warning: could not remove {path}: {error}", file=sys.stderr)


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


def resolve_from(root: Path, path: Path) -> Path:
    return path if path.is_absolute() else (root / path)


def cargo_home() -> Path:
    return Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")


def target_dir(root: Path) -> Path:
    configured = os.environ.get("CARGO_TARGET_DIR")
    if not configured:
        return root / "target"
    path = Path(configured)
    return path if path.is_absolute() else (root / path)


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
    print("   $ " + shlex.join(command), flush=True)
    result = subprocess.run(command, check=False, cwd=cwd)
    if result.returncode != 0:
        raise GateError(
            f"`{shlex.join(command)}` exited {result.returncode}. Nothing was "
            "uploaded. Fix the package, run the gate again, and only then tag."
        )


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
