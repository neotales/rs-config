#!/usr/bin/env python3
"""Package-verification gate for a group release. It never publishes.

Why this exists
---------------
Every crate in this repository shares one version, and `neotales-config-keepass`
depends on `neotales-config` at that version. Cargo resolves a dependency from
the registry while it prepares a package for publication, so the provider cannot
be packaged until the core is on crates.io. A release that learns that only
after publishing the core is half-released: a crates.io version is immutable, so
the provider could never be published at that version and the tag was spent.

`gate` stages the exact artifacts a release would publish in a local Cargo
registry and packages every member against it, in dependency order, before
anything is published. The staged registry is Cargo's own `local-registry`
source kind: a directory of `.crate` files and an index, not a stub that
pretends to be a registry. Dependencies come from the committed `Cargo.lock` and
are checked against its checksums; the participating members come from
`cargo package`, the same command `cargo publish` runs. The verification is
offline, with crates.io replaced by the staging directory, so a failure here is
a packaging failure and never a network failure.

Automated publishing is disabled on purpose
-------------------------------------------
This program never publishes. It has no subcommand that uploads, it does not
read a registry token, and it does not call the crates.io write API. Publishing
is a manual step, documented in RELEASE.md, because no local construction can
prove that the bytes this program verified are the bytes crates.io received:

* `cargo publish` does not upload `target/package/<name>-<version>.crate`. It
  repackages the checkout into its own target directory and uploads that
  repackaging. `cargo publish --dry-run` proves only that the packaging was
  byte-identical at that moment; the real invocation packages again and sends
  whatever that second packaging produces. Hashing the dry run is not a
  guarantee about the upload.
* Driving `PUT /api/v1/crates/new` directly would put the registry token in
  this automation, which is deliberately outside its reach, and the Cargo
  documentation of that endpoint states that the crates.io restrictions it
  lists "should not be considered an exhaustive list". A hand-written client
  cannot be proven correct without publishing for real, and a failed experiment
  spends an immutable version.

What `gate` and `preflight` therefore do is produce evidence and report, never
send. `gate` writes one machine-readable record: the commit it ran on, whether
that checkout was clean, and for every member the artifact filename, the
manifest digest, and the SHA-256 of the verified `.crate` bytes. It also prints
the SHA-256 of the record itself, and can write that digest to a separate path
for an out-of-band handoff. `preflight` requires that digest, and reads the
record once: the digest is checked over those bytes and those same bytes are
then parsed, so a record swapped between a would-be hash read and a would-be
parse read is neither checked-unparsed nor parsed-unchecked. It then reads the
recorded commit, the manifests and the artifacts, rejects a stale commit, a
changed manifest, a missing artifact, or an artifact whose bytes differ from the
record, and only then asks crates.io about the recorded digests and prints the
manual plan. It never re-hashes an artifact and presents the new hash as the
reference: a mismatch is a failure, not a new capture. The digest anchor binds
the record to the gate run only when the expected value comes from outside the
workspace that holds the record and artifacts; it is not tamper-proofing.
`preflight` does not run `cargo package`; the artifacts it reports on are the
ones the gate built.

The comparison remains an after-the-fact one. It does not prove that the bytes
`cargo publish` sends are the bytes the gate verified, and nothing here claims
that guarantee.

Usage
-----
    stage_gate.py gate [--root DIR] [--record PATH] [--record-sha256-out PATH]
                       [--self-test] [--keep-work]

        Package and verify every member in dependency order. Leaves the
        release artifacts in target/package/ and writes the captured digests
        to `--record`, by default `target/package/gate-record.json`. The record
        is written atomically and is the only input `preflight` treats as
        authoritative. Prints the SHA-256 of the record it just wrote and, with
        `--record-sha256-out`, writes that digest to a path the caller can hand
        to `preflight` out of band. `--self-test` additionally injects the
        failures this gate exists to catch, checks the verdicts of `state`,
        proves both packages are byte-stable, mutates an artifact and the record
        together and requires preflight to refuse them against the captured
        digest, swaps a different record in between the digest read and the
        parse and requires both `consume_record` and `recheck_record` to parse
        the bytes they checked, proves there is no publishing path, and runs the
        gate from outside the repository root. Nothing is published and no
        registry token is read.

    stage_gate.py state --manifest PATH --artifact-dir DIR [--index-base URL]

        Print one word describing whether crates.io already holds the version
        in PATH at the exact bytes in the artifact: `absent`, `identical`,
        `different` or `yanked`. `yanked` means the checksum matches but the
        version is yanked, which still blocks a release. Use this to check a
        downloaded copy of a manual publish against the recorded digest.

    stage_gate.py preflight --record PATH --expected-record-sha256 HEX
                            --artifact-dir DIR [--root DIR] [--index-base URL]

        Consume the gate record. The record bytes must hash to the digest
        supplied out of band by the gate run before this one; that digest is
        verified before the record is parsed, so a record and artifacts mutated
        together are still refused. Then reject a stale commit, a changed
        manifest, a missing artifact or an artifact that no longer hashes to the
        recorded digest. Then query crates.io with the recorded digests and
        print the manual plan. It does not repackage and does not re-hash an
        artifact into a new reference. Exit non-zero when a member is published
        at different bytes, is yanked, the group state is
        inconsistent, or the record no longer describes this checkout. Exit zero
        when the remaining work is a manual publish.
"""

from __future__ import annotations

import argparse
import hashlib
import io
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
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

# Cargo's default sparse index. It is read-only here: this program never talks to
# the crates.io write API, never authenticates and never reads a registry token.
INDEX_BASE = "https://index.crates.io/"

# Package directories in dependency order. The gate packages each one after the
# previous ones are in the staged registry, so this list is also the release
# order: `config/` is `neotales-config`, `keepass/` is
# `neotales-config-keepass`. A new member is added here in the same commit that
# adds it to the workspace, exactly like the map in the version job.
MEMBERS = ("config", "keepass")

WORK_PREFIX = "release-gate-"

# The gate record is a small, strict JSON document. `format` exists so a future
# change can reject an old record instead of guessing what it meant.
RECORD_FORMAT = 1
RECORD_FILENAME = "gate-record.json"
RECORD_KEYS = {"format", "commit", "clean", "members"}
RECORD_MEMBER_KEYS = {
    "name",
    "version",
    "artifact",
    "sha256",
    "manifest",
    "manifest_sha256",
}

ABSENT = "absent"
IDENTICAL = "identical"
DIFFERENT = "different"
YANKED = "yanked"


class GateError(Exception):
    """A failure that should stop the release, carrying what to do about it."""


@dataclass(frozen=True)
class ArtifactRecord:
    """One member as the gate captured it.

    `sha256` is the reference. It is written once by `gate` and read by
    `preflight`; a later hash of the file is only ever compared with it, never
    promoted to a new reference.
    """

    name: str
    version: str
    artifact: str
    sha256: str
    manifest: str
    manifest_sha256: str


@dataclass(frozen=True)
class ReleaseRecord:
    """The durable output of `gate`, and the only authority `preflight` uses."""

    commit: str
    clean: bool
    members: tuple[ArtifactRecord, ...]


@dataclass(frozen=True)
class Capture:
    """The bytes one member was verified as, read exactly once.

    The digest is the reference for every later comparison. The artifact file
    is mutable: a later `cargo package`, a manual edit, or a second process can
    rewrite it. Recording the digest at capture time is what makes the rest of
    the run talk about the bytes the gate verified rather than about whatever
    the file happens to contain now.
    """

    name: str
    version: str
    path: Path
    digest: str


@dataclass(frozen=True)
class MemberState:
    """What the registry says about one captured artifact."""

    name: str
    version: str
    expected: str
    state: str
    published: str | None = None
    yanked: bool = False


@dataclass(frozen=True)
class ReleasePlan:
    """The manual publishes a release still needs, in dependency order.

    `pending` is empty exactly when every member is already on crates.io at its
    captured digest. `blocking` is non-empty exactly when the release must not
    proceed, and `pending` is empty in that case too, so a caller cannot read a
    blocked plan as permission to publish the members that happen to be absent.
    """

    members: tuple[MemberState, ...]
    pending: tuple[str, ...]
    blocking: tuple[str, ...]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Package a group release against a staged registry, record "
        "the verified artifacts, and report the manual publishing plan. It "
        "never publishes."
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
        "--record",
        type=Path,
        default=None,
        help=(
            "where to write the captured digests; default "
            f"<target>/package/{RECORD_FILENAME}. Keep it out of the tracked "
            "tree: preflight rejects a checkout with untracked changes."
        ),
    )
    gate.add_argument(
        "--self-test",
        action="store_true",
        help="inject the failures the gate exists to catch and check they fail",
    )
    gate.add_argument(
        "--record-sha256-out",
        type=Path,
        default=None,
        help=(
            "write only the SHA-256 of the record to this path. The digest is "
            "computed from the exact bytes just written. Hand it to preflight "
            "out of band (a CI step output or the operator's notes), never from "
            "a file the record and artifacts can be changed with."
        ),
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
        "preflight",
        help="consume the gate record and report the manual plan",
    )
    add_root_argument(preflight)
    preflight.add_argument(
        "--record",
        type=Path,
        required=True,
        help="gate record to consume; its digests are the only reference",
    )
    preflight.add_argument(
        "--expected-record-sha256",
        required=True,
        help=(
            "SHA-256 the gate printed for this record when it wrote it, "
            "captured out of band (a CI step output or the operator's notes). "
            "The record bytes are checked against it before the record is "
            "parsed or the registry is queried."
        ),
    )
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

    arguments = parser.parse_args(argv)
    index_base = arguments.index_base.rstrip("/") + "/"

    try:
        if arguments.command == "gate":
            root = arguments.root.resolve()
            record = arguments.record
            if record is None:
                record = target_dir(root) / "package" / RECORD_FILENAME
            else:
                record = resolve_from(root, record)
            gate_release(
                root,
                index_base,
                record=record,
                record_sha256_out=(
                    None
                    if arguments.record_sha256_out is None
                    else resolve_from(root, arguments.record_sha256_out)
                ),
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
        else:
            root = arguments.root.resolve()
            preflight_release(
                root,
                resolve_from(root, arguments.record),
                resolve_from(root, arguments.artifact_dir),
                index_base,
                expected_record_sha256=arguments.expected_record_sha256,
            )
    except GateError as error:
        print(f"error: {error}", file=sys.stderr)
        if arguments.command == "preflight":
            print("Nothing was published.", file=sys.stderr)
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
    root: Path,
    index_base: str,
    *,
    record: Path,
    record_sha256_out: Path | None,
    self_test: bool,
    keep_work: bool,
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

        # Read the commit once. The artifacts cargo package writes carry this
        # same commit in `.cargo_vcs_info.json`; the record must name the commit
        # those bytes were built from, not whatever HEAD is when the record is
        # written.
        commit = git_head(root)

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
                    f"cargo package did not leave {artifact}; the artifact this "
                    "checkout built cannot be inspected"
                )
            digest = sha256(artifact)
            verify_artifact_provenance(artifact, name, version, commit)
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
            "candidate artifacts. Nothing was published and no registry token "
            "was read."
        )

        value = build_record(root, artifacts, commit=commit)
        record_digest = write_record(record, value)
        if read_record(record) != value:
            raise GateError(
                f"{record} did not read back as the captures the gate just "
                "verified; refusing to leave a record preflight could misread"
            )
        print(f"captured digests for {len(value.members)} members in {record}")
        print(f"record sha256: {record_digest}")
        if record_sha256_out is not None:
            write_record_digest(record_sha256_out, record_digest)
            print(f"record sha256 written to {record_sha256_out}")

        if self_test:
            self_check(root, work, artifacts, index_base, record)
    except Exception:
        print(f"gate work kept at {work}", file=sys.stderr)
        raise
    else:
        if keep_work:
            print(f"gate work kept at {work}")
        else:
            remove_owned_quietly(work, target)


# --------------------------------------------------------------------------
# the gate record: durable, machine-readable captures
# --------------------------------------------------------------------------


def build_record(
    root: Path,
    artifacts: list[tuple[str, str, Path, str]],
    *,
    commit: str | None = None,
) -> ReleaseRecord:
    """Build the record from the artifacts the gate just verified.

    The commit and the manifest digests are captured at the same time as the
    artifact digests. That is what lets `preflight` prove the record describes
    this checkout, rather than whatever happens to be on disk when it runs.
    """
    if len(artifacts) != len(MEMBERS):
        raise GateError(
            f"the gate verified {len(artifacts)} artifacts but the workspace has "
            f"{len(MEMBERS)} members; refusing to write an incomplete record"
        )

    commit = commit or git_head(root)
    clean = not git_status(root)
    if not clean:
        print(
            "warning: the checkout has uncommitted changes. The record names "
            f"commit {commit}, but preflight will reject it until the tree is "
            "clean; do not release from this record.",
            file=sys.stderr,
        )

    members: list[ArtifactRecord] = []
    for directory, (name, version, artifact, digest) in zip(MEMBERS, artifacts):
        # A clean tree at record time does not make a package built from a dirty
        # tree releasable: the dirty flag is stamped into the .crate bytes. Check
        # the stamp here too, so the record is never built from such bytes even
        # if a caller skipped the gate's earlier check.
        verify_artifact_provenance(artifact, name, version, commit)
        manifest = root / directory / "Cargo.toml"
        manifest_name, manifest_version = manifest_identity(manifest)
        if (manifest_name, manifest_version) != (name, version):
            raise GateError(
                f"the artifact {artifact.name} claims {name} {version}, but "
                f"{manifest} says {manifest_name} {manifest_version}. The gate "
                "must not write a record that mixes two versions."
            )
        expected_name = f"{name}-{version}.crate"
        if artifact.name != expected_name:
            raise GateError(
                f"the artifact {artifact.name} is not named {expected_name}; the "
                "record would not name the file preflight should verify"
            )
        members.append(
            ArtifactRecord(
                name=name,
                version=version,
                artifact=artifact.name,
                sha256=digest,
                manifest=relative_path(root, manifest),
                manifest_sha256=sha256(manifest),
            )
        )
    return ReleaseRecord(commit=commit, clean=clean, members=tuple(members))


def serialize_record(record: ReleaseRecord) -> bytes:
    """The exact bytes of a gate record; the digest is computed from these."""
    document = {
        "format": RECORD_FORMAT,
        "commit": record.commit,
        "clean": record.clean,
        "members": [
            {
                "name": member.name,
                "version": member.version,
                "artifact": member.artifact,
                "sha256": member.sha256,
                "manifest": member.manifest,
                "manifest_sha256": member.manifest_sha256,
            }
            for member in record.members
        ],
    }
    return (json.dumps(document, indent=2, sort_keys=True) + "\n").encode("utf-8")


def write_record(path: Path, record: ReleaseRecord) -> str:
    """Write the record atomically and return the SHA-256 of the bytes written.

    Hashing the serialized bytes before the write, rather than re-reading the
    file, means the digest the gate prints is the digest of this record even if
    something changes the path immediately afterwards. `preflight` compares the
    record with that digest before it parses anything.
    """
    if path.is_symlink():
        raise GateError(
            f"refusing to write the gate record through the symbolic link {path}"
        )
    data = serialize_record(record)
    digest = hashlib.sha256(data).hexdigest()
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        Path(temporary).unlink(missing_ok=True)
        raise

    # Make the rename durable where the platform allows it. A directory cannot
    # be opened this way on Windows; the file itself is already synced there.
    try:
        directory = os.open(path.parent, os.O_RDONLY)
    except OSError:
        return digest
    try:
        os.fsync(directory)
    except OSError:
        # Some platforms do not fsync directories. The file is already synced.
        pass
    finally:
        os.close(directory)
    return digest


def write_record_digest(path: Path, digest: str) -> None:
    """Write only the record digest, for an out-of-band handoff.

    This file is not an authority by itself: it is written in the same run that
    wrote the record. The caller must capture it somewhere the record and
    artifacts cannot also be changed (a CI step output, or the operator's own
    notes) and pass that value to `preflight --expected-record-sha256`.
    """
    if not is_digest(digest):
        raise GateError(f"refusing to write invalid record digest {digest!r}")
    if path.is_symlink():
        raise GateError(
            f"refusing to write the record digest through the symbolic link {path}"
        )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(digest + "\n", encoding="utf-8", newline="\n")


# Injection point for `--self-test`, and None in every real run. It is called
# after the record bytes have been read and verified and before those same bytes
# are parsed. `read_checked_record` hands the parser the buffer the digest
# covered, so a hook that replaces the file here proves the parser never reads
# the path again: whatever it parses has already been checked.
_record_swap_hook: Callable[[Path], None] | None = None


def read_record_bytes(path: Path) -> bytes:
    """Read the record's bytes, exactly once, for both hashing and parsing.

    A digest computed from one read and a JSON parser fed from another would let
    the file be swapped in between: the digest would describe bytes that were
    never parsed, and the parsed bytes would never have reached the digest
    comparison. The caller passes this buffer to both, so the bytes that are
    checked are the bytes that are parsed.
    """
    if path.is_symlink():
        raise GateError(f"{path} is a symbolic link; a gate record must be a real file")
    if not path.is_file():
        raise GateError(
            f"{path} is missing. preflight consumes the gate record; run "
            "`stage_gate.py gate --record PATH` first. It will not re-hash the "
            "artifacts and call that a capture."
        )
    try:
        return path.read_bytes()
    except OSError as error:
        raise GateError(f"{path} is not a readable gate record: {error}") from error


def read_record(path: Path) -> ReleaseRecord:
    """Read and strictly validate the gate record."""
    return parse_record(path, read_record_bytes(path))


def parse_record(path: Path, data: bytes) -> ReleaseRecord:
    """Parse and strictly validate record bytes the caller has already read.

    The bytes are an argument, not a path, so the parser cannot re-read a file
    that changed after its digest was checked.

    An unknown key is a failure, not something to ignore: `preflight` must not
    act on a record whose meaning it does not fully understand.
    """
    try:
        document = json.loads(data.decode("utf-8"))
    except (UnicodeError, ValueError) as error:
        raise GateError(f"{path} is not a readable gate record: {error}") from error
    if not isinstance(document, dict):
        raise GateError(f"{path} is not a JSON object")
    if set(document) != RECORD_KEYS:
        raise GateError(
            f"{path} has keys {sorted(document)}; expected {sorted(RECORD_KEYS)}"
        )
    if document["format"] != RECORD_FORMAT:
        raise GateError(
            f"{path} is format {document['format']!r}; this program reads format "
            f"{RECORD_FORMAT}. Re-run the gate."
        )
    commit = document["commit"]
    if not isinstance(commit, str) or not is_commit(commit):
        raise GateError(f"{path} has no valid commit: {commit!r}")
    clean = document["clean"]
    if not isinstance(clean, bool):
        raise GateError(f"{path} has no boolean clean flag: {clean!r}")
    entries = document["members"]
    if not isinstance(entries, list) or not entries:
        raise GateError(f"{path} has no members list")

    members: list[ArtifactRecord] = []
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            raise GateError(f"{path} member {index} is not a JSON object")
        if set(entry) != RECORD_MEMBER_KEYS:
            raise GateError(
                f"{path} member {index} has keys {sorted(entry)}; expected "
                f"{sorted(RECORD_MEMBER_KEYS)}"
            )
        for key in ("name", "version", "artifact", "manifest"):
            value = entry[key]
            if not isinstance(value, str) or not value:
                raise GateError(f"{path} member {index} has no {key}")
        for key in ("sha256", "manifest_sha256"):
            value = entry[key]
            if not isinstance(value, str) or not is_digest(value):
                raise GateError(
                    f"{path} member {entry['name']} has no valid {key}: {value!r}"
                )
        artifact = entry["artifact"]
        if Path(artifact).name != artifact or "/" in artifact or "\\" in artifact:
            raise GateError(
                f"{path} member {entry['name']} artifact {artifact!r} is not a "
                "plain filename"
            )
        members.append(
            ArtifactRecord(
                name=entry["name"],
                version=entry["version"],
                artifact=artifact,
                sha256=entry["sha256"],
                manifest=entry["manifest"],
                manifest_sha256=entry["manifest_sha256"],
            )
        )

    names = [member.name for member in members]
    if len(set(names)) != len(names):
        raise GateError(f"{path} lists a member more than once: {names}")
    return ReleaseRecord(commit=commit, clean=clean, members=tuple(members))


def validate_commit(root: Path, record: ReleaseRecord) -> None:
    """The record must describe the commit this checkout is on, cleanly."""
    current = git_head(root)
    if record.commit != current:
        raise GateError(
            f"the gate record is stale: it captured commit {record.commit}, but "
            f"{root} is at {current}. Re-run `stage_gate.py gate` on the commit "
            "being released. preflight will not re-hash the artifacts and treat "
            "the new bytes as the capture."
        )
    if not record.clean:
        raise GateError(
            "the gate record was written from a dirty checkout at commit "
            f"{record.commit}. It cannot describe a tag. Re-run the gate from a "
            "clean checkout of the tag."
        )


def validate_manifests(root: Path, record: ReleaseRecord) -> None:
    """Every member's manifest must still be the one the gate recorded."""
    if len(record.members) != len(MEMBERS):
        raise GateError(
            f"the gate record lists {len(record.members)} members, but this "
            f"workspace has {len(MEMBERS)}. Re-run the gate on this checkout."
        )

    for index, directory in enumerate(MEMBERS):
        manifest = root / directory / "Cargo.toml"
        name, version = manifest_identity(manifest)
        member = record.members[index]
        expected_manifest = relative_path(root, manifest)
        if member.manifest != expected_manifest:
            raise GateError(
                f"the gate record member {index} names the manifest "
                f"{member.manifest!r}, but workspace member {directory}/ is "
                f"{expected_manifest!r}. The record is from another tree."
            )
        if (member.name, member.version) != (name, version):
            raise GateError(
                f"the gate record says {member.name} {member.version}, but "
                f"{manifest} says {name} {version}. The manifest changed after "
                "the gate ran; the recorded artifact is not this crate."
            )
        actual = sha256(manifest)
        if actual != member.manifest_sha256:
            raise GateError(
                f"{manifest} hashes to {actual}, but the gate record captured "
                f"{member.manifest_sha256}. The manifest changed after the gate "
                "verified it; re-run the gate from the checkout being released."
            )


def validate_checkout(root: Path, record: ReleaseRecord) -> None:
    """Refuse a working tree that moved after the record was written."""
    status = git_status(root)
    if status:
        raise GateError(
            "the checkout has changed since the gate ran:\n  "
            + "\n  ".join(status.splitlines())
            + "\nThe record describes a clean checkout of "
            f"{record.commit}. Commit or discard these changes and re-run the gate."
        )


def verify_record_artifacts(
    record: ReleaseRecord,
    artifact_dir: Path,
) -> tuple[Capture, ...]:
    """Compare each artifact with the gate record; never re-hash as authority.

    The returned `Capture` carries the recorded digest. A file that no longer
    matches is an error, not a new capture, so every later comparison still
    talks about the bytes the gate verified.
    """
    captures: list[Capture] = []
    for member in record.members:
        expected = f"{member.name}-{member.version}.crate"
        if member.artifact != expected:
            raise GateError(
                f"the gate record lists artifact {member.artifact!r} for "
                f"{member.name} {member.version}, expected {expected!r}"
            )
        path = artifact_dir / member.artifact
        if not path.is_file():
            raise GateError(
                f"{path} is missing. The gate recorded {member.name} "
                f"{member.version} at sha256 {member.sha256}; preflight reports "
                "only on artifacts that still exist. Re-run `stage_gate.py gate` "
                "from a clean checkout of the tag."
            )
        current = sha256(path)
        if current != member.sha256:
            raise GateError(
                f"{path} hashes to {current}, but the gate recorded "
                f"{member.sha256} for {member.name} {member.version}. The "
                "artifact changed after the gate verified it; the recorded digest "
                "no longer describes that file. Refusing to re-hash it into a new "
                "reference. Re-run `stage_gate.py gate` from a clean checkout of "
                "the tag."
            )
        captures.append(Capture(member.name, member.version, path, member.sha256))
    return tuple(captures)


def verify_record_bytes(path: Path, data: bytes, expected: str) -> None:
    """Compare these already-read record bytes with the digest the gate captured.

    `data` is exactly the buffer the caller will parse. It is not hashed again
    from the path, so a record changed between a hash read and a parse read
    cannot pass. The expected value must come from outside the workspace that
    holds the record and artifacts; that separation is what stops a record and
    its artifacts being replaced together.
    """
    if not is_digest(expected):
        raise GateError(
            f"--expected-record-sha256 {expected!r} is not a 64-character "
            "lowercase SHA-256 digest. Pass the exact value the gate printed "
            "when it wrote the record."
        )
    actual = hashlib.sha256(data).hexdigest()
    if actual != expected:
        raise GateError(
            f"the gate record {path} hashes to {actual}, not the digest "
            f"{expected} captured when the gate wrote it. The record changed "
            "after the gate ran, so preflight will not parse it or ask the "
            "registry anything. Re-run the gate and use the digest that run "
            "printed (in CI, the gate step's output)."
        )


def read_checked_record(path: Path, expected: str) -> ReleaseRecord:
    """Read the record once, verify those bytes, and parse those same bytes.

    The digest check and the parser share one buffer, so a record swapped on
    disk after the read cannot be parsed. A record changed before the read is
    refused against the captured digest without being interpreted.
    """
    data = read_record_bytes(path)
    verify_record_bytes(path, data, expected)
    if _record_swap_hook is not None:
        _record_swap_hook(path)
    return parse_record(path, data)


def consume_record(
    root: Path,
    record_path: Path,
    artifact_dir: Path,
    expected_record_sha256: str,
) -> tuple[ReleaseRecord, tuple[Capture, ...]]:
    """Load and fully validate the record before any plan is considered.

    The record digest is verified first, on the same bytes that are then
    parsed. Without it, a record and its artifacts rewritten together would
    agree with each other and preflight could not tell the pair from the gate's
    output.
    """
    record = read_checked_record(record_path, expected_record_sha256)
    validate_commit(root, record)
    validate_manifests(root, record)
    validate_checkout(root, record)
    captures = verify_record_artifacts(record, artifact_dir)
    return record, captures


def recheck_record(
    root: Path,
    record_path: Path,
    record: ReleaseRecord,
    artifact_dir: Path,
    expected_record_sha256: str,
) -> None:
    """Re-prove the record before a human acts on the plan."""
    current = read_checked_record(record_path, expected_record_sha256)
    if current != record:
        raise GateError(
            f"the gate record {record_path} changed while preflight was running. "
            "Nothing was published; re-run the gate and preflight."
        )
    validate_commit(root, current)
    validate_manifests(root, current)
    validate_checkout(root, current)
    verify_record_artifacts(current, artifact_dir)


def relative_path(root: Path, path: Path) -> str:
    """A stable record path: relative to the root when possible, POSIX style."""
    resolved = path.resolve()
    try:
        return resolved.relative_to(root.resolve()).as_posix()
    except ValueError:
        return resolved.as_posix()


def is_digest(value: str) -> bool:
    return len(value) == 64 and all(
        character in "0123456789abcdef" for character in value
    )


def is_commit(value: str) -> bool:
    return len(value) in (40, 64) and all(
        character in "0123456789abcdef" for character in value
    )


def git_output(root: Path, *arguments: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(root), *arguments],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise GateError(
            f"`git -C {root} {shlex.join(arguments)}` exited "
            f"{result.returncode}: {detail}. The gate record names the commit it "
            "describes, so a release must run in a Git checkout."
        )
    return result.stdout.strip()


def git_head(root: Path) -> str:
    commit = git_output(root, "rev-parse", "HEAD")
    if not is_commit(commit):
        raise GateError(f"{root} did not report a commit: {commit!r}")
    return commit


def git_status(root: Path) -> str:
    return git_output(root, "status", "--porcelain", "--untracked-files=normal")


def artifact_provenance(artifact: Path, name: str, version: str) -> tuple[str, bool]:
    """Read the commit and dirty flag `cargo package` stamped into the crate.

    The stamp is what ties the `.crate` bytes to a commit. The gate compares it
    with the commit the record will name, so the record cannot mix an artifact
    from one commit with a record from another.
    """
    member = f"{name}-{version}/.cargo_vcs_info.json"
    try:
        with tarfile.open(artifact, "r:gz") as archive:
            extracted = archive.extractfile(member)
            if extracted is None:
                raise GateError(
                    f"{artifact} has no {member}. cargo package did not stamp the "
                    "commit, so these bytes cannot be tied to the checkout."
                )
            document = json.loads(extracted.read().decode("utf-8"))
    except (tarfile.TarError, OSError, UnicodeError, ValueError) as error:
        raise GateError(
            f"{artifact} is not a readable crate archive: {error}"
        ) from error

    git = document.get("git") if isinstance(document, dict) else None
    if not isinstance(git, dict):
        raise GateError(f"{artifact} has no git information in {member}")
    commit = git.get("sha1")
    dirty = git.get("dirty", False)
    if not isinstance(commit, str) or not is_commit(commit):
        raise GateError(f"{artifact} has no valid commit in {member}: {commit!r}")
    if not isinstance(dirty, bool):
        raise GateError(
            f"{artifact} has a non-boolean dirty flag in {member}: {dirty!r}"
        )
    return commit, dirty


def verify_artifact_provenance(
    artifact: Path,
    name: str,
    version: str,
    commit: str,
) -> None:
    """Refuse an artifact stamped from another commit or a dirty tree.

    The dirty check is not made redundant by a clean checkout at record time.
    `cargo package` stamps `dirty` when it builds the package, so reverting or
    committing the changes afterwards leaves a `.crate` whose own bytes say it
    was built from a dirty tree. Such a package must never enter the record.
    """
    packaged_commit, packaged_dirty = artifact_provenance(artifact, name, version)
    if packaged_commit != commit:
        raise GateError(
            f"{artifact.name} was packaged from commit {packaged_commit}, "
            f"but the gate is recording {commit}. The artifact and the record "
            "would name different commits; refusing to write it."
        )
    if packaged_dirty:
        raise GateError(
            f"{artifact.name} was packaged from a dirty tree: its "
            ".cargo_vcs_info.json says dirty=true. A clean checkout now does not "
            "change those bytes. Commit or discard the changes and re-run the "
            "gate from a clean checkout; a package stamped dirty is not "
            "releasable."
        )


def restamp_crate(
    source: Path,
    destination: Path,
    name: str,
    version: str,
    commit: str,
    *,
    dirty: bool,
) -> None:
    """Copy a crate archive, replacing only its `.cargo_vcs_info.json`.

    The self-test uses this to build a coherent sandbox: artifact stamps are
    rewritten to the sandbox commit, and a deliberately dirty stamp is used to
    prove the gate refuses a package built from a dirty tree even after that tree
    is clean again.
    """
    stamp = f"{name}-{version}/.cargo_vcs_info.json"
    payload = json.dumps(
        {
            "pkg_schema_version": 1,
            "git": {"sha1": commit, "dirty": dirty},
        },
        separators=(",", ":"),
    ).encode("utf-8")
    with (
        tarfile.open(source, "r:gz") as archive,
        tarfile.open(destination, "w:gz") as output,
    ):
        for member in archive.getmembers():
            if member.name == stamp:
                info = tarfile.TarInfo(stamp)
                info.size = len(payload)
                info.mode = member.mode
                info.mtime = member.mtime
                info.uid = member.uid
                info.gid = member.gid
                info.uname = member.uname
                info.gname = member.gname
                output.addfile(info, io.BytesIO(payload))
                continue
            extracted = archive.extractfile(member)
            output.addfile(member, extracted)


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


def classify_capture(
    capture: Capture,
    index_base: str,
    *,
    quiet: bool = False,
) -> MemberState:
    """Classify the captured digest against the registry. Read-only.

    The artifact file is not read here. The comparison is between the registry
    checksum and the digest recorded when the artifact was verified, so a file
    rewritten after capture cannot make a spent version look identical.
    """
    name, version, expected = capture.name, capture.version, capture.digest
    line = index_line(fetch_index(name, index_base), name, version)
    if line is None:
        if not quiet:
            print(
                f"{name} {version} is not published; the verified "
                f"{capture.path.name} (sha256 {expected}) is not on crates.io",
                file=sys.stderr,
            )
        return MemberState(name, version, expected, ABSENT)

    published_entry = json.loads(line)
    published = published_entry.get("cksum")
    yanked = bool(published_entry.get("yanked", False))
    if published == expected:
        if yanked:
            if not quiet:
                print(
                    f"crates.io holds {name} {version} at sha256 {expected}, the "
                    "verified artifact, but the version is yanked. A yanked version "
                    "is not a completed release and cannot be published again.",
                    file=sys.stderr,
                )
            return MemberState(
                name, version, expected, YANKED, published=published, yanked=True
            )
        if not quiet:
            print(
                f"crates.io holds {name} {version} at sha256 {expected}, the "
                "verified artifact",
                file=sys.stderr,
            )
        return MemberState(name, version, expected, IDENTICAL, published=published)

    if not quiet:
        suffix = " The published version is also yanked." if yanked else ""
        print(
            f"crates.io holds {name} {version} at sha256 "
            f"{published or '<no checksum>'}, but the verified artifact hashes to "
            f"{expected}.{suffix}",
            file=sys.stderr,
        )
    return MemberState(
        name, version, expected, DIFFERENT, published=published, yanked=yanked
    )


def artifact_state(manifest: Path, artifact_dir: Path, index_base: str) -> str:
    """One-word verdict for a single artifact, using a digest read once."""
    name, version = manifest_identity(manifest)
    path = artifact_dir / f"{name}-{version}.crate"
    if not path.is_file():
        raise GateError(
            f"{path} is missing. Build it with `stage_gate.py gate` before asking "
            "crates.io about it."
        )
    capture = Capture(name, version, path, sha256(path))
    member = classify_capture(capture, index_base)
    return member.state


def preflight_release(
    root: Path,
    record_path: Path,
    artifact_dir: Path,
    index_base: str,
    *,
    expected_record_sha256: str,
) -> ReleasePlan:
    """Consume the gate record and report the manual plan.

    The record is first checked against the digest the gate printed when it
    wrote that record. The digest must have been captured out of band; a record
    and its artifacts rewritten together then hash to something else and are
    refused before the record is parsed. The record is validated in full before
    anything else: commit, clean flag, manifests, and artifact bytes. A stale
    record or a changed artifact is an error, never a new capture. Nothing is
    published. A blocking registry state is an error; a member that is merely
    absent is work for a human, reported with the commands and the digest to
    check afterwards.
    """
    require_repository(root)
    record, captures = consume_record(
        root,
        record_path,
        artifact_dir,
        expected_record_sha256,
    )

    print(
        f"gate record: {record_path} (commit {record.commit}, clean "
        f"{str(record.clean).lower()})"
    )
    print("recorded artifacts (sha256 from the gate, compared with the files):")
    for capture in captures:
        print(
            f"  {capture.name} {capture.version}  sha256 {capture.digest}  "
            f"({capture.path})"
        )

    members = [
        classify_capture(capture, index_base, quiet=True) for capture in captures
    ]
    plan = decide(members)
    for member in plan.members:
        detail = {
            ABSENT: "not published; a manual publish is required",
            IDENTICAL: "already published at the recorded checksum",
            YANKED: "published at the recorded checksum but yanked",
            DIFFERENT: "published at different bytes",
        }.get(member.state, "unknown")
        print(f"{member.name} {member.version}: {member.state} ({detail})")

    if plan.blocking:
        lines = ["release blocked. Nothing was published:"]
        lines.extend(f"  - {reason}" for reason in plan.blocking)
        lines.append(
            "This workflow has no publishing step. Fix the crates.io state or bump "
            "the version in every member manifest, then run again."
        )
        raise GateError("\n".join(lines))

    # Read the registry a second time with the same recorded digests. If it
    # changed while the plan was built, the plan is stale and a human must look
    # again rather than act on it.
    after = [classify_capture(capture, index_base, quiet=True) for capture in captures]
    before_state = tuple((m.state, m.published, m.yanked) for m in plan.members)
    after_state = tuple((m.state, m.published, m.yanked) for m in after)
    if before_state != after_state:
        raise GateError(
            "crates.io changed while preflight was running:\n"
            f"  before: {before_state}\n  after:  {after_state}\n"
            "Nothing was published. Re-run the workflow to get a consistent plan."
        )

    # The plan is only meaningful for the bytes and checkout the record
    # describes. Re-prove that; do not re-hash the artifacts into a new
    # reference.
    recheck_record(
        root,
        record_path,
        record,
        artifact_dir,
        expected_record_sha256,
    )

    if plan.pending:
        print_manual_plan(plan)
    else:
        print(
            "plan: nothing to publish. Every member is on crates.io at the "
            "recorded digest."
        )
    return plan


def print_manual_plan(plan: ReleasePlan) -> None:
    """Print the human procedure. This program never executes it."""
    print(
        "\npublishing is manual: this program never uploads, never reads a "
        "registry token and has no publishing subcommand. system-admin performs "
        "the uploads and alone handles CARGO_REGISTRY_TOKEN."
    )
    pending = set(plan.pending)
    print("remaining members, in dependency order:")
    for member in plan.members:
        if member.name not in pending:
            continue
        print(f"  cargo publish --package {member.name} --locked")
        print(f"    afterwards the published artifact must hash to {member.expected}")
    print(
        "After each upload, download the .crate from static.crates.io and compare "
        "its SHA-256 with the digest above. The core comes first: if its download "
        "does not match, stop before uploading the provider. The comparison is "
        "after the fact and does not prove which bytes cargo publish sent. "
        "RELEASE.md section 'Publishing by hand' has the commands and the "
        "spent-version rule."
    )


def decide(members: list[MemberState]) -> ReleasePlan:
    """Turn registry states into the manual work, or into blocking reasons.

    This is deliberately pure: it is the function the self-test injects a
    mismatched or yanked provider into, and it must never report pending work
    when any member blocks the release. It schedules nothing itself; there is no
    publishing call in this program.
    """
    blocking: list[str] = []
    pending: list[str] = []

    for member in members:
        if member.state == ABSENT:
            pending.append(member.name)
        elif member.state == IDENTICAL:
            continue
        elif member.state == YANKED:
            blocking.append(
                f"{member.name} {member.version} is published at the recorded "
                "checksum but is yanked. A yanked version is not installable and "
                "cannot be published again. If the yank was ours, unyank it on "
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
    # registry, not as a green light to publish the core around it.
    absent: str | None = None
    for member in members:
        if member.state == ABSENT:
            absent = member.name if absent is None else absent
        elif absent is not None:
            blocking.append(
                f"{member.name} {member.version} is published while {absent} is not. "
                "The group state is inconsistent; nothing may be published until "
                "the registry agrees on the group."
            )

    if blocking:
        return ReleasePlan(tuple(members), (), tuple(blocking))
    return ReleasePlan(tuple(members), tuple(pending), ())


# --------------------------------------------------------------------------
# self-test: the failures this gate exists to catch
# --------------------------------------------------------------------------


def self_check(
    root: Path,
    work: Path,
    artifacts: list[tuple[str, str, Path, str]],
    index_base: str,
    record_path: Path,
) -> None:
    """Inject the failures the gate must catch, and the verdicts `state` returns.

    These are the release's own failure paths, so they are exercised on every CI
    run rather than trusted. Nothing is published and no token is read.
    """
    core_name, core_version, core_artifact, core_digest = artifacts[0]
    last_name, _last_version, _last_artifact, last_digest = artifacts[-1]

    print("\n== self-test: failure injection")

    outside = Path(tempfile.mkdtemp(prefix="stage-gate-outside-"))
    previous = Path.cwd()
    try:
        os.chdir(outside)
        try:
            # 1. The pre-fix blocker: the core is not in the registry at all.
            missing = copy_registry(work / "registry", work / "inject-missing-core")
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
            drifted = copy_registry(work / "registry", work / "inject-version-drift")
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

    # 3. The state verdicts and the manual plan, against a local index built from
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
    captures = {
        name: Capture(name, version, artifact, digest)
        for name, version, artifact, digest in artifacts
    }

    def state_of(name: str) -> MemberState:
        return classify_capture(captures[name], base, quiet=True)

    def plan_now() -> ReleasePlan:
        return decide([state_of(core_name), state_of(last_name)])

    expect_state(
        IDENTICAL,
        state_of(core_name).state,
        description="an artifact that matches the published checksum",
    )
    expect_state(
        IDENTICAL,
        state_of(last_name).state,
        description="the provider matching its published checksum",
    )
    expect_plan(
        plan_now(),
        pending=(),
        description="both members already published at the gate checksums",
    )

    rewrite_entry(index, last_name, cksum="0" * 64)
    expect_state(
        DIFFERENT,
        state_of(last_name).state,
        description="a provider whose bytes differ from the published checksum",
    )
    expect_blocked(
        plan_now(),
        "a provider published at different bytes",
    )

    rewrite_entry(index, last_name, cksum=last_digest, yanked=True)
    expect_state(
        YANKED,
        state_of(last_name).state,
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
        state_of(core_name).state,
        description="a core whose bytes differ from the published checksum",
    )
    expect_blocked(
        plan_now(),
        "a core published at different bytes",
    )

    rewrite_entry(index, core_name, cksum=core_digest, yanked=True)
    expect_state(
        YANKED,
        state_of(core_name).state,
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
        state_of(last_name).state,
        description="a provider version the index does not list",
    )
    expect_plan(
        plan_now(),
        pending=(last_name,),
        description="recovery: the core is published and the provider is absent",
    )

    # 4. The record contract: a sandbox with a real Git commit, the same
    #    manifests and copies of the verified artifacts. Preflight accepts the
    #    gate's record, then each input is changed in turn and preflight must
    #    refuse before it prints a plan. The same CLI is also driven through the
    #    absent/different/yanked registry states, and never uploads anything.
    verify_record_between_gate_and_preflight(
        root,
        work,
        artifacts,
        original,
        core_name,
        last_name,
    )

    mirror_state_index(index, original, core_name, last_name)
    remove_index(index, last_name)
    remove_index(index, core_name)
    expect_state(
        ABSENT,
        state_of(core_name).state,
        description="a core version the index does not list",
    )
    expect_plan(
        plan_now(),
        pending=(core_name, last_name),
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
        pending=(),
        description="both members restored at the gate checksums",
    )

    # 5. A package stamped dirty must never enter the record, even when the tree
    #    is clean by the time the record is built.
    verify_dirty_provenance(root, work, artifacts)

    # 6. The gate artifact is byte-stable for BOTH packages, and there is no
    #    publishing path in this program.
    verify_package_stability(root, work, artifacts)
    verify_no_publish_path(root)

    # 7. Unsafe cleanup guards.
    test_cleanup_guards()

    # 8. The full gate from outside the repository root. This is a subprocess, so
    #    it also exercises the default --root and every path argument from a cwd
    #    that is not the checkout.
    verify_gate_from_outside(root, artifacts, index_base, record_path)

    print(
        "self-test passed: the gate rejects a missing or drifted core; both "
        "members are byte-stable; a package stamped dirty is refused even after "
        "the tree is clean; the gate record is rejected when its commit, manifest "
        "or artifact changes between gate and preflight, and a record and "
        "artifact mutated together are rejected against the digest captured when "
        "the gate wrote the record; a different record swapped in between the "
        "digest read and the parse is never the record that gets parsed, by "
        "consume_record or recheck_record; mismatched and matching-but-yanked "
        "states block with zero pending publishes, and this program has no "
        "publishing path."
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


def verify_dirty_provenance(
    root: Path,
    work: Path,
    artifacts: list[tuple[str, str, Path, str]],
) -> None:
    """A package stamped dirty stays dirty after the tree is clean again.

    The sandbox has a clean Git checkout, but the core artifact's
    `.cargo_vcs_info.json` is restamped `dirty=true` at that same commit. This is
    what a package built from uncommitted changes and then committed or reverted
    looks like: `git status` is empty, while the `.crate` bytes still say the
    build was dirty. `build_record` must refuse it, because a clean tree at
    record time cannot retroactively clean the package.
    """
    sandbox = work / "dirty-provenance"
    if sandbox.exists() or sandbox.is_symlink():
        raise GateError(
            f"self-test: refusing to reuse {sandbox}; the dirty-package check "
            "needs a directory this run created"
        )
    sandbox.mkdir()
    try:
        for directory in MEMBERS:
            target = sandbox / directory
            target.mkdir()
            shutil.copy2(root / directory / "Cargo.toml", target / "Cargo.toml")
        init_git_repository(sandbox)
        commit = git_head(sandbox)
        if git_status(sandbox):
            raise GateError("self-test: the dirty-package sandbox is not clean")

        name, version, artifact, _digest = artifacts[0]
        last_name, last_version, last_artifact, _last_digest = artifacts[-1]

        artifact_dir = sandbox / "target" / "package"
        artifact_dir.mkdir(parents=True)
        dirty_core = artifact_dir / f"{name}-{version}.crate"
        restamp_crate(artifact, dirty_core, name, version, commit, dirty=True)
        clean_last = artifact_dir / f"{last_name}-{last_version}.crate"
        restamp_crate(
            last_artifact,
            clean_last,
            last_name,
            last_version,
            commit,
            dirty=False,
        )

        # The tree is clean now. Only the artifact stamp carries the dirtiness.
        if git_status(sandbox):
            raise GateError(
                "self-test: the dirty-package tree is not clean after restamping"
            )
        try:
            build_record(
                sandbox,
                [
                    (name, version, dirty_core, sha256(dirty_core)),
                    (
                        last_name,
                        last_version,
                        clean_last,
                        sha256(clean_last),
                    ),
                ],
                commit=commit,
            )
        except GateError as error:
            assert_message(
                str(error),
                "dirty=true",
                description="a package stamped dirty is named as the reason",
            )
        else:
            raise GateError(
                "self-test: build_record accepted a package stamped dirty after "
                "the tree became clean"
            )
        print("   ok: a package stamped dirty is refused after the tree is clean")

        # The same artifacts with an honest stamp build a record, so the refusal
        # above is about the stamp and not a broken sandbox.
        clean_dir = artifact_dir / "clean-stamps"
        clean_dir.mkdir()
        clean_core = clean_dir / f"{name}-{version}.crate"
        restamp_crate(artifact, clean_core, name, version, commit, dirty=False)
        record = build_record(
            sandbox,
            [
                (name, version, clean_core, sha256(clean_core)),
                (
                    last_name,
                    last_version,
                    clean_last,
                    sha256(clean_last),
                ),
            ],
            commit=commit,
        )
        if not record.clean:
            raise GateError("self-test: the clean-stamp sandbox record is dirty")
        print("   ok: the same artifacts with a clean stamp build a record")
    finally:
        remove_owned(sandbox, work)


def verify_record_between_gate_and_preflight(
    root: Path,
    work: Path,
    artifacts: list[tuple[str, str, Path, str]],
    original: dict[str, str],
    core_name: str,
    last_name: str,
) -> None:
    """A mutation between gate and preflight must be refused, not re-hashed.

    The sandbox has a real Git commit, the same manifests and copies of the
    verified artifacts. The record is produced by the gate's own writer. Then
    each input is changed after the record exists: an artifact, a missing
    artifact, a manifest, the commit, and a source file. Every one must make
    preflight exit non-zero before it prints a plan, and none may turn the new
    bytes into a new reference.
    """
    sandbox = work / "record-contract"
    if sandbox.exists() or sandbox.is_symlink():
        raise GateError(
            f"self-test: refusing to reuse {sandbox}; the record contract needs a "
            "directory this run created"
        )
    sandbox.mkdir()
    try:
        shutil.copy2(root / "Cargo.toml", sandbox / "Cargo.toml")
        shutil.copy2(root / "Cargo.lock", sandbox / "Cargo.lock")
        for directory in MEMBERS:
            target = sandbox / directory
            target.mkdir()
            shutil.copy2(root / directory / "Cargo.toml", target / "Cargo.toml")

        init_git_repository(sandbox)
        sandbox_commit = git_head(sandbox)

        # The real artifacts carry the real repository's commit. Restamp the
        # copies to the sandbox commit so the record contract talks about one
        # coherent tree, and recompute their digests from the restamped bytes.
        artifact_dir = sandbox / "target" / "package"
        artifact_dir.mkdir(parents=True)
        copied: list[tuple[str, str, Path, str]] = []
        for name, version, artifact, _digest in artifacts:
            copy = artifact_dir / f"{name}-{version}.crate"
            restamp_crate(
                artifact,
                copy,
                name,
                version,
                sandbox_commit,
                dirty=False,
            )
            copied.append((name, version, copy, sha256(copy)))

        # The index lines carry the real repository's checksums. The restamped
        # copies hash differently, so build sandbox-local lines from the copied
        # digests; otherwise preflight would correctly report the registry bytes
        # as different from the record the sandbox is testing.
        cksums = {name: digest for name, _version, _path, digest in copied}
        sandbox_original: dict[str, str] = {}
        for member_name in (core_name, last_name):
            entry = json.loads(original[member_name])
            entry["cksum"] = cksums[member_name]
            sandbox_original[member_name] = json.dumps(entry, separators=(",", ":"))

        record = build_record(sandbox, copied, commit=sandbox_commit)
        if not record.clean:
            raise GateError(
                "self-test: the record contract sandbox is dirty before the record "
                "was written"
            )
        record_path = work / "record-contract.json"
        record_digest = write_record(record_path, record)
        if read_record(record_path) != record:
            raise GateError("self-test: the gate record did not round-trip")

        outside = work / "record-contract-outside"
        outside.mkdir()
        index = work / "record-contract-index"
        index.mkdir()
        for member_name in (core_name, last_name):
            append_index(index, member_name, sandbox_original[member_name])
        base = index.resolve().as_uri().rstrip("/") + "/"

        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        if status != 0:
            raise GateError(
                "self-test: preflight rejected a gate record that describes the "
                f"checkout:\n{output}"
            )
        assert_message(
            output,
            "gate record",
            description="preflight says which record it consumed",
        )
        assert_message(
            output,
            "nothing to publish",
            description="preflight reports both members at the recorded digest",
        )
        print("   ok: preflight consumes the gate record and prints its digests")

        # The digest check and the parser must read the record once. A swap in
        # the window between a would-be hash read and a would-be parse read must
        # not change what is parsed, for both record consumers.
        verify_record_bytes_are_the_parsed_bytes(
            sandbox,
            record_path,
            record,
            record_digest,
            artifact_dir,
        )

        core_record = record.members[0]
        last_record = record.members[-1]
        if (core_record.name, last_record.name) != (core_name, last_name):
            raise GateError("self-test: the record members are not in release order")

        original_record_bytes = record_path.read_bytes()
        if sha256(record_path) != record_digest:
            raise GateError(
                "self-test: the record digest does not describe the record the "
                "gate just wrote"
            )

        # A record that no longer hashes to the captured value must be refused
        # before it is parsed. Corrupting the JSON proves the digest check runs
        # ahead of the parser rather than only after a successful read.
        record_path.write_bytes(b"{ not a gate record")
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        expect_preflight_refusal(
            status,
            output,
            "captured when the gate wrote it",
            description="a record changed into invalid JSON after the gate",
        )
        assert_not_message(
            output,
            "not a readable gate record",
            description="the record digest is checked before parsing",
        )
        record_path.write_bytes(original_record_bytes)
        print(
            "   ok: a changed record is refused against the captured digest "
            "before it is parsed"
        )

        # Change an artifact and rewrite the record so the two agree. Without an
        # independently captured record digest this is a consistent pair: every
        # recorded digest matches the file beside it. The captured digest is the
        # only input that still ties the record to the gate run.
        forged_artifact = artifact_dir / core_record.artifact
        original_artifact_bytes = forged_artifact.read_bytes()
        forged_artifact.write_bytes(
            original_artifact_bytes + b"\nforged between gate and preflight\n"
        )
        forged_digest = sha256(forged_artifact)
        forged = json.loads(original_record_bytes)
        for entry in forged["members"]:
            if entry["name"] == core_record.name:
                entry["sha256"] = forged_digest
        record_path.write_text(
            json.dumps(forged, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        if record_digest == sha256(record_path):
            raise GateError("self-test: the forged record hash is unchanged")
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        expect_preflight_refusal(
            status,
            output,
            "captured when the gate wrote it",
            description="a record and artifact mutated together",
        )
        assert_not_message(
            output,
            "publishing is manual",
            description="a forged record never reaches a plan",
        )
        record_path.write_bytes(original_record_bytes)
        forged_artifact.write_bytes(original_artifact_bytes)
        print(
            "   ok: a record and artifact mutated together are refused against "
            "the digest captured from the gate"
        )

        # The artifact changes after the gate wrote the record. Preflight must
        # compare with the record, not adopt the new hash.
        mutated = artifact_dir / core_record.artifact
        preserved = mutated.read_bytes()
        with mutated.open("ab") as handle:
            handle.write(b"mutated between gate and preflight")
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        expect_preflight_refusal(
            status,
            output,
            "changed after the gate verified it",
            description="an artifact mutated between gate and preflight",
        )
        assert_message(
            output,
            core_record.sha256,
            description="the refusal names the digest the gate recorded",
        )
        mutated.write_bytes(preserved)
        print(
            "   ok: an artifact mutated between gate and preflight is refused "
            "before any plan"
        )

        mutated.unlink()
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        expect_preflight_refusal(
            status,
            output,
            "is missing",
            description="an artifact removed between gate and preflight",
        )
        mutated.write_bytes(preserved)
        print("   ok: a missing artifact is refused before any plan")

        manifest = sandbox / "config" / "Cargo.toml"
        manifest_text = manifest.read_text(encoding="utf-8")
        manifest.write_text(
            manifest_text + "\n# changed after the gate\n", encoding="utf-8"
        )
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        expect_preflight_refusal(
            status,
            output,
            "manifest changed after the gate",
            description="a manifest changed between gate and preflight",
        )
        manifest.write_text(manifest_text, encoding="utf-8")
        print("   ok: a manifest changed after the gate is refused before any plan")

        source = sandbox / "config" / "src" / "lib.rs"
        source.parent.mkdir()
        source.write_text("pub fn changed() {}\n", encoding="utf-8")
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        expect_preflight_refusal(
            status,
            output,
            "checkout has changed since the gate ran",
            description="a source file changed between gate and preflight",
        )
        source.unlink()
        source.parent.rmdir()
        print("   ok: a changed checkout is refused before any plan")

        (sandbox / "stale.txt").write_text("a newer commit\n", encoding="utf-8")
        commit_all(sandbox, "move HEAD after the gate record")
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        expect_preflight_refusal(
            status,
            output,
            "gate record is stale",
            description="a record from a different commit",
        )
        assert_message(
            output,
            "commit",
            description="the refusal says the commit does not match",
        )
        print("   ok: a record from another commit is refused before any plan")
        run_git(sandbox, "reset", "--hard", record.commit)

        # The registry states, driven through the same CLI. Each run consumes
        # the same record and never uploads.
        mirror_state_index(index, sandbox_original, core_name, last_name)
        remove_index(index, last_name)
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        if status != 0:
            raise GateError(
                "self-test: preflight treated one absent member as a failure:\n"
                + output
            )
        assert_message(
            output,
            "publishing is manual",
            description="the plan says publishing is manual",
        )
        assert_message(
            output,
            f"cargo publish --package {last_name}",
            description="the plan names the member a human must publish",
        )
        assert_message(
            output,
            last_record.sha256,
            description="the plan prints the recorded digest to compare afterwards",
        )
        assert_message(
            output,
            "system-admin",
            description="the plan says who supplies the token",
        )
        assert_message(
            output,
            "RELEASE.md",
            description="the plan points at the documented procedure",
        )
        assert_not_message(
            output,
            "Uploading",
            description="the plan publishes nothing",
        )
        print(
            f"   ok: preflight reports {last_name} as manual work and publishes nothing"
        )

        for injected, needle, description in (
            ({"cksum": "0" * 64, "yanked": False}, "different", "different bytes"),
            (
                {"cksum": last_record.sha256, "yanked": True},
                "yanked",
                "matching but yanked",
            ),
        ):
            mirror_state_index(index, sandbox_original, core_name, last_name)
            rewrite_entry(index, last_name, **injected)
            status, output = run_preflight_record(
                sandbox, record_path, record_digest, artifact_dir, base, outside
            )
            expect_preflight_refusal(
                status,
                output,
                needle,
                description=f"a provider published at {description}",
            )

        mirror_state_index(index, sandbox_original, core_name, last_name)
        remove_index(index, core_name)
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        expect_preflight_refusal(
            status,
            output,
            "inconsistent",
            description="a provider published while the core is absent",
        )

        mirror_state_index(index, sandbox_original, core_name, last_name)
        remove_index(index, core_name)
        remove_index(index, last_name)
        status, output = run_preflight_record(
            sandbox, record_path, record_digest, artifact_dir, base, outside
        )
        if status != 0:
            raise GateError(
                "self-test: preflight treated a fresh group as a failure:\n" + output
            )
        assert_message(
            output,
            f"cargo publish --package {core_name}",
            description="a fresh group plans the core first",
        )
        assert_message(
            output,
            f"cargo publish --package {last_name}",
            description="a fresh group plans the provider second",
        )
        assert_not_message(
            output,
            "Uploading",
            description="a fresh group still publishes nothing",
        )
        print("   ok: a fresh group prints the ordered manual plan and no upload")
    finally:
        remove_owned(sandbox, work)


def verify_record_bytes_are_the_parsed_bytes(
    root: Path,
    record_path: Path,
    record: ReleaseRecord,
    record_digest: str,
    artifact_dir: Path,
) -> None:
    """The digest check and the parser must consume one read of the record.

    Reading the record once for the digest and again for JSON is a window: the
    file can be replaced in between, so the comparison passes on bytes that were
    never parsed and the parser accepts bytes that were never compared. The
    self-test hook fires inside that window and replaces the record with a
    different, still well-formed one whose commit would fail validation if it
    were ever parsed. Both consumers must return the record they verified, not
    the file the hook wrote.
    """
    global _record_swap_hook

    original = record_path.read_bytes()
    if hashlib.sha256(original).hexdigest() != record_digest:
        raise GateError(
            "self-test: the record does not hash to the digest captured for it"
        )
    replacement = serialize_record(
        ReleaseRecord(
            commit="0" * len(record.commit),
            clean=record.clean,
            members=record.members,
        )
    )
    if replacement == original:
        raise GateError("self-test: the substituted record is not different")
    if hashlib.sha256(replacement).hexdigest() == record_digest:
        raise GateError(
            "self-test: the substituted record hashes to the captured digest, so "
            "the swap would be invisible"
        )

    swapped: list[Path] = []

    def swap(path: Path) -> None:
        path.write_bytes(replacement)
        if path.read_bytes() != replacement:
            raise GateError("self-test: the record swap did not reach the file")
        swapped.append(path)

    try:
        _record_swap_hook = swap
        consumed, captures = consume_record(
            root, record_path, artifact_dir, record_digest
        )
    except GateError as error:
        raise GateError(
            "self-test: consume_record parsed a record that was swapped in after "
            f"its digest was checked: {error}"
        ) from error
    finally:
        _record_swap_hook = None

    if len(swapped) != 1:
        raise GateError(
            "self-test: consume_record no longer reads and verifies the record in "
            "one step, so the swap window is not observable"
        )
    if record_path.read_bytes() != replacement:
        raise GateError(
            "self-test: the record was not swapped while consume_record ran"
        )
    if consumed != record:
        raise GateError(
            "self-test: consume_record parsed bytes its digest check did not cover"
        )
    if tuple(capture.digest for capture in captures) != tuple(
        member.sha256 for member in record.members
    ):
        raise GateError(
            "self-test: consume_record did not report the recorded artifact digests"
        )
    record_path.write_bytes(original)

    swapped.clear()
    try:
        _record_swap_hook = swap
        recheck_record(root, record_path, record, artifact_dir, record_digest)
    except GateError as error:
        raise GateError(
            "self-test: recheck_record parsed a record that was swapped in after "
            f"its digest was checked: {error}"
        ) from error
    finally:
        _record_swap_hook = None

    if len(swapped) != 1:
        raise GateError(
            "self-test: recheck_record no longer reads and verifies the record in "
            "one step, so the swap window is not observable"
        )
    if record_path.read_bytes() != replacement:
        raise GateError(
            "self-test: the record was not swapped while recheck_record ran"
        )
    record_path.write_bytes(original)
    print(
        "   ok: the digest check and the parser consume one read, so a record "
        "swapped in between is neither parsed by consume_record nor recheck_record"
    )


def run_preflight_record(
    root: Path,
    record_path: Path,
    expected_record_sha256: str,
    artifact_dir: Path,
    base: str,
    cwd: Path,
) -> tuple[int, str]:
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "preflight",
        "--root",
        str(root),
        "--record",
        str(record_path),
        "--expected-record-sha256",
        expected_record_sha256,
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
    return result.returncode, result.stdout + result.stderr


def expect_preflight_refusal(
    status: int,
    output: str,
    needle: str,
    *,
    description: str,
) -> None:
    if status == 0:
        raise GateError(
            f"self-test: preflight accepted {description}, but it must refuse"
        )
    assert_message(output, needle, description=f"preflight names {description}")
    assert_message(
        output,
        "Nothing was published",
        description="preflight says nothing was published",
    )
    assert_not_message(
        output,
        "publishing is manual",
        description="preflight printed no manual plan",
    )
    assert_not_message(
        output,
        "nothing to publish",
        description="preflight printed no successful plan",
    )
    assert_not_message(
        output,
        "Uploading",
        description="preflight did not reach a publishing path",
    )
    print(f"   ok: preflight refuses {description} from outside the repository")


def init_git_repository(root: Path) -> None:
    run_git(root, "init", "-q")
    (root / ".gitignore").write_text("/target\n", encoding="utf-8")
    commit_all(root, "self-test: record contract")


def commit_all(root: Path, message: str) -> None:
    run_git(root, "add", "-A")
    environment = dict(os.environ)
    environment.update(
        {
            "GIT_AUTHOR_NAME": "stage-gate self-test",
            "GIT_AUTHOR_EMAIL": "stage-gate-self-test@example.invalid",
            "GIT_COMMITTER_NAME": "stage-gate self-test",
            "GIT_COMMITTER_EMAIL": "stage-gate-self-test@example.invalid",
        }
    )
    result = subprocess.run(
        [
            "git",
            "-C",
            str(root),
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=environment,
    )
    if result.returncode != 0:
        raise GateError(
            f"self-test: cannot commit the record contract sandbox: "
            f"{(result.stderr or result.stdout).strip()}"
        )


def run_git(root: Path, *arguments: str) -> None:
    result = subprocess.run(
        ["git", "-C", str(root), *arguments],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if result.returncode != 0:
        raise GateError(
            f"self-test: `git -C {root} {shlex.join(arguments)}` exited "
            f"{result.returncode}: {(result.stderr or result.stdout).strip()}"
        )


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


def verify_no_publish_path(root: Path) -> None:
    """The program must have no way to upload, and no way to read a token."""
    source = Path(__file__).resolve().read_text(encoding="utf-8")
    # Built by concatenation so this check does not match its own list.
    forbidden = {
        "def publish" + "_release(": "a publishing function",
        '"' + "publish" + '",': "a cargo publish invocation",
        "execute" + "_plan(": "an upload scheduler",
        "urllib.request." + "Request(": "a write request to the registry API",
        '"' + "Author" + "ization" + '"': "registry authentication",
        "DEFAULT_" + "PUBLISH_TIMEOUT": "an upload confirmation wait",
    }
    for marker, why in forbidden.items():
        if marker in source:
            raise GateError(
                f"self-test: {Path(__file__).name} still contains {marker!r}, "
                f"which is {why}"
            )
    print("   ok: the gate has no publishing function, request or token read")

    outside = Path(tempfile.mkdtemp(prefix="stage-gate-publish-outside-"))
    try:
        environment = dict(os.environ)
        # A refusal must happen in the parser, before anything could read a
        # token. Remove any inherited token rather than read it.
        environment.pop("CARGO_REGISTRY_TOKEN", None)
        # Split so the no-publishing-path scan above does not match this test.
        verb = "pub" + "lish"
        result = subprocess.run(
            [
                sys.executable,
                str(Path(__file__).resolve()),
                verb,
                "--root",
                str(root),
                "--manifest",
                str(root / "config" / "Cargo.toml"),
                "--artifact-dir",
                str(target_dir(root) / "package"),
            ],
            check=False,
            cwd=outside,
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=environment,
        )
        output = result.stdout + result.stderr
        if result.returncode == 0:
            raise GateError("self-test: the publish subcommand was accepted")
        assert_message(output, "invalid choice", description="publish is not a command")
        assert_not_message(output, "Uploading", description="no publishing code ran")
        print("   ok: the publish subcommand is rejected before any action")
    finally:
        remove_owned(outside, outside.parent)


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
            raise GateError("self-test: cleanup removed a path outside its owned root")
        if not outside_file.is_file():
            raise GateError("self-test: cleanup deleted an unowned file")
    finally:
        remove_owned(sandbox, sandbox.parent)


def verify_gate_from_outside(
    root: Path,
    artifacts: list[tuple[str, str, Path, str]],
    index_base: str,
    record_path: Path,
) -> None:
    outside = Path(tempfile.mkdtemp(prefix="stage-gate-outside-gate-"))
    digest_path = record_path.with_name(record_path.name + ".sha256")
    try:
        command = [
            sys.executable,
            str(Path(__file__).resolve()),
            "gate",
            "--root",
            str(root),
            "--index-base",
            index_base,
            "--record",
            str(record_path),
            "--record-sha256-out",
            str(digest_path),
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
        written = read_record(record_path)
        expected = {name: digest for name, _version, _artifact, digest in artifacts}
        for member in written.members:
            if member.sha256 != expected.get(member.name):
                raise GateError(
                    f"self-test: the outside-root gate recorded {member.name} at "
                    f"{member.sha256}, not {expected.get(member.name)}"
                )
        if not digest_path.is_file():
            raise GateError(
                "self-test: the gate did not write the record digest it was asked for"
            )
        handoff = digest_path.read_text(encoding="utf-8").strip()
        actual = sha256(record_path)
        if handoff != actual:
            raise GateError(
                f"self-test: the gate wrote record digest {handoff}, but the "
                f"record hashes to {actual}; preflight would reject its own run"
            )
        assert_message(
            output,
            f"record sha256: {actual}",
            description="the gate printed the digest of the record it wrote",
        )
        print(
            "   ok: the gate ran from outside the repository root, reproduced "
            "both artifacts, rewrote the record and handed off its digest"
        )
    finally:
        digest_path.unlink(missing_ok=True)
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
    pending: tuple[str, ...],
    description: str,
) -> None:
    if plan.blocking:
        raise GateError(
            f"self-test: {description} was blocked: {'; '.join(plan.blocking)}"
        )
    if plan.pending != pending:
        raise GateError(
            f"self-test: {description} planned {plan.pending}, expected {pending}"
        )
    print(f"   ok: {description} plans {pending or 'no manual publishes'}")


def expect_blocked(plan: ReleasePlan, description: str) -> None:
    if not plan.blocking:
        raise GateError(f"self-test: {description} was not blocked")
    if plan.pending:
        raise GateError(
            f"self-test: {description} was blocked but still planned publishes "
            f"{plan.pending}"
        )
    print(f"   ok: {description} is blocked with zero pending publishes")


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
            "published. Fix the package, run the gate again, and only then tag."
        )


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
