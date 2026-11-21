# Releasing

**Automated publishing is disabled.** `release.yml` verifies a tag and records
what a release would contain. It has no publishing step, holds no credential, and
cannot upload. Publishing is a manual step, described in
[Publishing by hand](#publishing-by-hand).

The automation was removed because it promised more than it could prove.
`cargo publish` does not upload `target/package/<name>-<version>.crate`: it
repackages the checkout into its own target directory and sends that
repackaging. A preceding `cargo publish --dry-run` shows that the packaging was
byte-identical at that moment, not that the second packaging — the one whose
bytes are sent — is. Replacing `cargo publish` with a direct call to the
crates.io upload API would put the registry token inside the automation, and
still could not be proven without a real publish: the Cargo documentation of
that endpoint states that the crates.io restrictions it lists are not
exhaustive. Either mistake spends an immutable version, so neither is automated.

What remains is the part that can be proven locally: the group packages
together, in dependency order, and the digest of each verified artifact is
recorded once and compared with crates.io. A human publishes, then compares the
artifact downloaded from crates.io with that recorded digest.

## What runs on a tag

`release.yml` runs on a push of a `v*` tag:

| Job | What it does |
| --- | --- |
| `check` | Format, clippy, tests, a `cargo publish --dry-run` packaging smoke test, and (on `workflow_dispatch` only) the gate and preflight. |
| `version` | Requires the tag to equal `v` plus the version in every member manifest. |
| `verify` | Runs the gate, records each artifact's SHA-256, asks crates.io (read-only) about those captured digests, proves the tree is the tag and unmodified, and states that nothing was published. |

The `verify` job has `permissions: contents: read`. It does not request
`id-token`, and there is no `CARGO_REGISTRY_TOKEN` secret in this repository.
A green run means **the tag is verified, not released**. The job summary carries
the recorded digests and the manual plan.

A `workflow_dispatch` run never reaches `verify`; it runs the checks, the gate
and preflight, and publishes nothing.

## The version rule

The tag is `v` plus the version in every crate manifest. Tag `v0.3.1` describes
the group only if `config/Cargo.toml` and `keepass/Cargo.toml` both say
`version = "0.3.1"`. The `version` job fails the run otherwise, so a tag can
never describe a version that differs from the tree. The job reads one manifest
per member, so a crate added to the workspace is added to that map at the same
time.

The members share one version. `neotales-config-keepass` depends on
`neotales-config` at that same version, which is why the core is published
first.

Bump the version, commit, push, then tag that commit:

```sh
python3 eng/release/stage_gate.py gate
python3 eng/release/stage_gate.py preflight --artifact-dir target/package
git tag v0.3.1
git push origin dev
git push origin v0.3.1
```

The tag run verifies the tag. It does not publish it. The first version of a new
crate is uploaded by hand and has no tag-driven history at all; see
[First publish of a new crate](#first-publish-of-a-new-crate).

## The gate that runs before anything is published

`neotales-config-keepass` depends on `neotales-config` at the group version, and
Cargo resolves a dependency from the registry while it prepares a package for
publication. Before the core is on crates.io there is no registry entry to
resolve, so the provider cannot be packaged. A release must not discover that
*after* publishing the core: a crates.io version is immutable, so the provider
would be unpublishable at that version and the tag would be spent.

`eng/release/stage_gate.py gate` closes that hole. It runs in `ci` and in the
`verify` job. It:

1. copies every crate in `Cargo.lock` out of the Cargo download cache and checks
   each one against the checksum in the lockfile;
2. writes a Cargo `local-registry` source over that directory and replaces
   `crates.io` with it, so the staged core is resolvable exactly as it will be
   after publication;
3. runs `cargo package` for each member in dependency order — the same build
   `cargo publish` performs — adding each member's packaged `.crate` to the
   staging registry before the next member is resolved;
4. fails the run if any member does not package and verify.

After staging, the verification is offline. `crates.io` is replaced by the
staging directory, so resolution cannot reach the network and a verification
failure is a packaging failure, not a flake. Staging itself needs the network
once, through `cargo fetch --locked`, which is also what a cold runner needs
before it can build anything.

What the gate proves: every member packages, and every member builds from the
exact `.crate` bytes the gate staged, against the members before it. What it does
not prove: that crates.io will accept the upload, that the bytes `cargo publish`
repackages later are identical, or that a published artifact matches. The first
is why a human publishes; the last two are why the recorded digest is compared
with the artifact downloaded from crates.io afterwards.

### The gate artifact is not the upload artifact

`cargo publish` repackages into its own target directory and uploads that
tarball. Nothing in this repository claims that the gate artifact is the upload
artifact. The gate artifact is the reference: its SHA-256 is recorded once by
`preflight`, and the same digest is what you check against the file crates.io
serves after a manual upload. This is a comparison after the fact, not a check
that bound the bytes that were sent, and the documentation says so plainly.

`preflight` reads each artifact exactly once. Every later decision — the
registry comparison, the second read of the registry, and the final plan — uses
that recorded digest, never the file again. If the artifact changes after it was
read (a second `cargo package`, a manual edit, another process), the run fails
with `the verified artifact changed after it was read` rather than reporting on
bytes it did not verify.

### Both members are checked, and nothing is published

`preflight` asks crates.io about both members using the recorded digests and
prints each member's state, then the group verdict:

| State | Meaning |
| --- | --- |
| `absent` | The version is not on crates.io. A manual publish is required. |
| `identical` | crates.io holds this version at the recorded digest. Nothing to do for this member. |
| `different` | The version is on crates.io at other bytes. The version is spent; bump it. |
| `yanked` (with a matching digest) | The version is not installable and cannot be published again. Unyank it if the yank was ours; otherwise bump. |
| inconsistent group | A later member is published while an earlier one is absent. Investigate; do not publish around it. |

`different`, `yanked`, and an inconsistent group exit non-zero and print
`Nothing was published`. `absent` exits zero with the manual plan: the member
names in dependency order, the `cargo publish` command to run, and the digest to
compare afterwards. The registry is read a second time with the same captured
digests; if it changed while the plan was built, the run fails and asks you to
re-run rather than acting on a stale plan.

## Publishing by hand

This is the only publishing path. It is a human operation. The crates.io token
(`CARGO_REGISTRY_TOKEN`) is supplied by `system-admin`; this repository, its
workflow and `stage_gate.py` never read it. Do not place the token in the
repository, on a command line, or in shell history.

Do all of this from a **clean checkout of the tag**, not from a working tree and
not from a branch:

```sh
git status --porcelain          # must be empty
git describe --tags --exact-match HEAD   # must print the tag
```

1. Verify the group and record the digests. The `verify` job prints them; run it
   locally if you need to:

   ```sh
   python3 eng/release/stage_gate.py gate
   python3 eng/release/stage_gate.py preflight --artifact-dir target/package
   ```

   `preflight` must exit zero. Write down the two `sha256` lines. If it exits
   non-zero because of `different`, `yanked` or an inconsistent group, stop:
   the version cannot be published as verified.

2. Publish the core first, with the token in the environment:

   ```sh
   cargo publish --package neotales-config --locked
   ```

3. Compare the published bytes with the recorded digest **before** touching the
   provider:

   ```sh
   name=neotales-config
   version=$(python3 -c 'import tomllib;print(tomllib.load(open("config/Cargo.toml","rb"))["package"]["version"])')
   curl -fsSL -o "/tmp/$name-$version.crate" \
     "https://static.crates.io/crates/$name/$name-$version.crate"
   sha256sum "/tmp/$name-$version.crate"
   python3 eng/release/stage_gate.py state \
     --manifest config/Cargo.toml --artifact-dir /tmp
   ```

   The `sha256sum` must equal the recorded digest for the core, and `state`
   points at the downloaded copy, so it confirms the registry's checksum
   describes that same file: it must print `identical`. If either check does
   not match, stop: the version is spent. Do not publish the provider, do not
   retry, and do not yank-and-republish. Bump the version in every manifest,
   commit, tag, and start again.

4. Publish the provider and compare it the same way:

   ```sh
   cargo publish --package neotales-config-keepass --locked
   name=neotales-config-keepass
   version=$(python3 -c 'import tomllib;print(tomllib.load(open("keepass/Cargo.toml","rb"))["package"]["version"])')
   curl -fsSL -o "/tmp/$name-$version.crate" \
     "https://static.crates.io/crates/$name/$name-$version.crate"
   sha256sum "/tmp/$name-$version.crate"
   python3 eng/release/stage_gate.py state \
     --manifest keepass/Cargo.toml --artifact-dir /tmp
   ```

5. Confirm the end state. Each downloaded `sha256sum` must equal its recorded
   digest, and each `state` must print `identical`. If you also want CI's view,
   run `preflight`; it re-packages the tree, so it compares a fresh build with
   the registry:

   ```sh
   python3 eng/release/stage_gate.py preflight --artifact-dir target/package
   ```

   Both members must report `identical` and the plan must say
   `nothing to publish`.

If `cargo publish` reports an error after crates.io accepted the upload (its own
confirmation wait can time out), do not re-run it. Ask the registry: `state`
prints `identical` if the bytes arrived. If they did not, or the digest differs,
the version is spent and the recovery is a bump.

## First publish of a new crate

The first version of a new crate is `0.0.0-alpha.0`. Not `0.0.0-alpha.1` and not
`0.1.0`. The first upload needs a version nothing else has claimed, and it is
manual, like every other upload now.

The first upload goes through `/home/dev/work/rs/publish.sh <dir>`. That script
lives beside this folder, in `~/work/rs`, and is deliberately not copied into the
crate repository: it is how the first version of every crate is uploaded, not
part of any one crate. The operator runs it; it never becomes part of CI, and
this repository never holds its token.

1. Set `version = "0.0.0-alpha.0"` in every member manifest that is being
   published together, commit it, and leave the working tree clean.
2. Run the gate and preflight before uploading anything:

   ```sh
   python3 eng/release/stage_gate.py gate
   python3 eng/release/stage_gate.py preflight --artifact-dir target/package
   ```

   The gate stages the candidate core locally and verifies the provider against
   it, and uploads nothing. `preflight` requires both versions to be absent (or
   already present at the recorded digest). A version spent at other bytes, or a
   matching-but-yanked version, stops here. If the gate fails, the manual upload
   would fail at the provider with the core already published, which is the
   state this repository's gate exists to avoid.

3. Publish the dependency first. This repository's core is `config/`:

   ```sh
   ~/work/rs/publish.sh config
   ~/work/rs/publish.sh keepass
   ```

   The provider's upload fails if `neotales-config` 0.0.0-alpha.0 is not on
   crates.io yet: `cargo publish` verifies the package against the registry, not
   against this checkout. That is the intended order.

4. Compare the downloaded artifacts with the gate digests, as in
   [Publishing by hand](#publishing-by-hand). `publish.sh` does not compare
   anything; the comparison is what tells you the upload matched the verified
   artifact.

If the crate name is taken on crates.io by someone else, stop. A rename is a
different repository setup, not a flag.

**Do not enable Trusted Publishing Only for these crates.** The only publisher is
a human with a token. A trusted publisher configured earlier is unused by
`release.yml`, which no longer requests a token; enabling the restriction would
break manual publishing without replacing it with anything proven.

`cargo package -p neotales-config` includes every tracked file under `config/`,
and `cargo package -p neotales-config-keepass` includes every tracked file under
`keepass/`, so each crate README, its licence, its docs, and its tests ship
inside its published crate. The root `RELEASE.md`, `.github/`, and the workspace
manifest stay out of both packages: they belong to the repository, not to a
crate. The README style guide lives at `~/work/rs/README.STYLE.md` and is not
part of a crate.

## Later releases

1. Bump the version in every member manifest that shares the group version, and
   in the provider's dependency on `neotales-config`, then commit. `Cargo.lock`
   is committed on purpose, so run `cargo build` and commit the updated lockfile
   with it. The lockfile is what the scheduled audit checks.
2. Push the commit.
3. Tag it `vX.Y.Z` and push the tag.
4. The workflow runs the checks and the gate, records the digests, reports the
   registry state and stops. Nothing is published.
5. Publish by hand, in dependency order, from a clean checkout of the tag, and
   compare each downloaded artifact with its recorded digest.

## When the core published and the provider did not

The group can be left half-published by a manual error, a registry failure, or a
token that expired between the two uploads. A crates.io version cannot be
replaced, so the core must not be published twice and the provider must not be
versioned on its own: the version rule requires both manifests to carry the
tag's version.

Ask the registry first:

```sh
python3 eng/release/stage_gate.py preflight --artifact-dir target/package
```

| Registry answer for the recorded digest | What to do |
| --- | --- |
| the core is `identical`, the provider is `absent` | This is the expected half-published state. Check out the same tag cleanly, publish the provider by hand, and compare the downloaded artifact with the recorded digest. Do not re-publish the core. |
| both are `identical` | The release is complete. Nothing to do. |
| any member is `different` | That version is spent. Bump the version in every manifest and tag again. Do not retry, and do not try to replace the version. |
| a member is `yanked` with a matching digest | The version is not installable and cannot be published again. Unyank it on crates.io if that was our yank; otherwise bump the version. |
| the provider is published while the core is absent | The registry state is inconsistent with a group release. Investigate the crate's version history and bump the group version. Do not publish around it. |

Re-running the workflow from the tag re-verifies and re-reports. It cannot
publish and cannot change the state above.

## What is not allowed

- No automated publishing. `release.yml` must not gain an upload step, a
  registry token, or `id-token: write`, and `stage_gate.py` must not gain a
  publishing command or a token read. The `release-gate` job in `ci.yml` checks
  that the program has neither.
- `pull_request_target` and `workflow_run` are not used anywhere, and must not be
  added. Both can run untrusted code with publishing credentials in scope.
- No publishing from a branch or a dirty working tree. The tag and a clean
  checkout are what make the recorded digests describe something nameable.
- No republishing a version. crates.io rejects it. If a publish fails after the
  upload was accepted, bump the version; never retry the same version.
- No claim that a manual upload matched without comparing the artifact
  downloaded from crates.io with the recorded digest. A version string alone
  cannot tell a completed release from a different tarball.
- No publishing a member after preflight blocked. The plan carries zero pending
  publishes when any member is mismatched, yanked, or inconsistent.
- No deleting a target directory the gate did not create, and no following a
  symlinked target to do it.
- No enabling Trusted Publishing Only on a crate that is published by hand.

## Cleanup safety

The gate stages into a unique `target/release-gate-*` directory it creates with
`mkdtemp` and owns. It refuses a `CARGO_TARGET_DIR` (or a `target/`) that
traverses a symbolic link, and every removal checks that the path is a real child
of the directory this run created. It never deletes a fixed `release-gate` name
that another checkout could have placed behind a symlink. There is no separate
publish work directory to leak, because there is no publish step.

`cargo package -p neotales-config-keepass` on its own still fails until the core
is published. That failure is expected and is not a gate. Use the gate, which
stages the core it needs.

## Failure modes

| Symptom | Cause and action |
| --- | --- |
| `version` job: tag does not match `vX.Y.Z` | A member manifest version and the tag disagree. Delete the tag, fix, re-tag. |
| Gate: a member does not package or verify | The crate's published form does not build, usually a dependency that is not on crates.io or a path-only dependency without a version. Nothing was published. Fix the crate and tag again. |
| Gate: `failed to select a version` for `neotales-config`, or `the lock file needs to be updated` | The provider's dependency version and the core manifest disagree, or the version bump did not update `Cargo.lock`. Nothing was published. Fix the versions, run `cargo build`, commit the lockfile, and tag again. |
| Gate: `no matching package named neotales-config` | The staged registry lost the core. That is a bug in the gate, not a release problem, and it is the state this gate exists to make impossible. The `release-gate` job in `ci.yml` injects this failure on every run. |
| Gate: `cannot read https://index.crates.io/...` | The index was unreachable while staging the locked dependencies. Nothing was published; re-run the job. |
| Gate: `... is a symbolic link` | `CARGO_TARGET_DIR` or `target/` traverses a symlink. The gate refuses to create or delete through it. Point the target at a real directory. Nothing was published. |
| Verify: `the checkout has changes` or `HEAD is ... not ...` | The tag job is not running on a clean checkout of that exact tag. The recorded digests would describe something else. Nothing was published; investigate the checkout, not the release. |
| Preflight: `different` | crates.io already holds this version at other bytes. It exits non-zero. A version cannot be replaced: bump the patch version and tag again. Do not retry the tag. |
| Preflight: `yanked` | crates.io holds this exact artifact but the version is yanked. It exits non-zero. Unyank it on crates.io if that was our yank; otherwise bump the version. A yanked version cannot be published again. |
| Preflight: provider exists while the core does not | The registry state is inconsistent with a group release. Nothing was published. Do not publish around it; investigate the versions and bump the group version. |
| Preflight: `the verified artifact changed after it was read` | Something rewrote `target/package/*.crate` after the digest was recorded. The recorded digest no longer describes the file. Re-run the gate from a clean tag checkout; do not publish bytes whose digest was not verified. |
| Preflight: `crates.io changed while preflight was running` | The registry state moved between two reads. Nothing was published. Re-run the workflow for a consistent plan. |
| `verify` is green but the crate is not on crates.io | This is the intended state: the tag is verified, not released. Publish by hand, then compare the downloaded artifact with the recorded digest. |
| Manual publish: `cargo publish` rejects the token | The token is missing or invalid, or the crate is in Trusted Publishing Only mode. Do not enable that mode while publishing by hand; ask `system-admin` for a token that can publish. |
| Manual publish: `cargo publish` reports an error but the artifact downloads and matches | crates.io accepted the upload and cargo failed later, usually its own confirmation wait. Investigate the cargo error; do not upload the version again. |
| Manual publish: the downloaded digest differs from the recorded digest | The version is spent. Do not publish the next member, do not retry, and do not yank-and-republish. Bump the version in every manifest and tag again. |
| Manual publish: `cargo publish` fails for the provider because the core is missing | The core was not published, or its version does not match the provider's dependency. Publish the core first, from the same tag. |
| `cargo package -p neotales-config-keepass` fails locally | The core version is not on crates.io, and Cargo resolves the path dependency from the registry while preparing the package, even with `--no-verify`. Run `python3 eng/release/stage_gate.py gate` instead; it stages the core and verifies the provider. |
