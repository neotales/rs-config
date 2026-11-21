# Releasing

Publishing runs from `release.yml`. It is driven by a tag and it authenticates
with a short-lived crates.io token obtained over OIDC. There is no
`CARGO_REGISTRY_TOKEN` secret in this repository.

## The version rule

The tag is `v` plus the version in every crate manifest. Tag `v0.3.1` publishes the group only if
`config/Cargo.toml` and `keepass/Cargo.toml` both say `version = "0.3.1"`. The `version` job fails
the run otherwise, so a tag can never publish a version that differs from what is in the tree. The
job reads one manifest per member, so a crate added to the workspace is added to that map at the
same time.

The members share one version. `neotales-config-keepass` depends on `neotales-config` at that same
version, which is why the core is published first.

Bump the version, commit, push, then tag that commit:

```sh
python3 eng/release/stage_gate.py gate
python3 eng/release/stage_gate.py preflight --artifact-dir target/package
git tag v0.3.1
git push origin dev
git push origin v0.3.1
```

The one exception is a new crate's first version, which is uploaded by hand and
never from a tag. That is the next section.

## The gate that runs before anything is uploaded

`neotales-config-keepass` depends on `neotales-config` at the group version, and Cargo resolves a
dependency from the registry while it prepares a package for upload. Before the core is on
crates.io there is no registry entry to resolve, so the provider cannot be packaged. The release
must not discover that *after* uploading the core: a crates.io version is immutable, so the
provider would be unpublishable at that version and the tag would be spent.

`eng/release/stage_gate.py gate` closes that hole. On a tag it runs in the publish job before the
token exchange; on a manual run it runs in the `check` job. It:

1. copies every crate in `Cargo.lock` out of the Cargo download cache and checks each one against
   the checksum in the lockfile;
2. writes a Cargo `local-registry` source over that directory and replaces `crates.io` with it, so
   the staged core is resolvable exactly as it will be after the upload;
3. runs `cargo package` for each member in dependency order — the same build `cargo publish`
   performs — adding each member's packaged `.crate` to the staging registry before the next
   member is resolved;
4. fails the run if any member does not package and verify.

After staging, the verification is offline. `crates.io` is replaced by the staging directory, so
resolution cannot reach the network and a verification failure is a packaging failure, not a flake.
Staging itself needs the network once, through `cargo fetch --locked`, which is also what a cold
runner needs before it can build anything.

What the gate proves: every member packages, and every member builds from the exact `.crate` bytes
the gate staged, against the members before it. What it does not prove: crate-name ownership, the
trusted publisher configuration, or that crates.io will accept the upload. Those remain the publish
job's failures, and the preflight and checksum decisions below are what make them recoverable.

### The gate artifact is not automatically the upload artifact

`cargo publish` does not upload `target/package/<name>-<version>.crate`. It repackages into its own
`--target-dir`, uploads that tarball, and computes the checksum of the bytes it sends. A gate that
only hashes `target/package/` would prove nothing about the upload.

`publish` closes that gap in two places, using the same `--target-dir` for both:

1. Before uploading, `cargo publish --dry-run` runs the publish path and leaves its `tmp-crate`
   tarball. The command compares that tarball's SHA-256 with the gate artifact. If the bytes differ,
   nothing is uploaded.
2. After uploading, the command compares the `tmp-crate` tarball cargo just uploaded (when it is
   still present) and requires crates.io's index checksum to equal the gate artifact. The registry
   checksum is the authoritative evidence; the local tarball is a second witness.

The core's proof runs before any upload. The provider's proof can only run after the core is on
crates.io, because the publish path resolves the core from the registry. It runs after the core
upload and before the provider upload. If it fails, the provider is not uploaded; the core is
published, and re-running the same tag skips the core and reaches the provider again. Nothing here
asserts that a file was uploaded without a checksum from crates.io to back it.

### Both members are checked before any upload

`preflight` asks crates.io about both members and fails the release when either version is already
published at different bytes, is published at the gate artifact's checksum but yanked, or the group
state is inconsistent (for example, a provider exists while the core does not). It runs in the
publish job before the OIDC token exchange, so a blocking state is found while the job holds no
credential. A manual run runs it in the `check` job as well.

`publish` rechecks both members at the start and again after its dry-run packaging step, so the last
check is immediately before the upload. Those checks and the upload are separate requests, so a
version can still be yanked or taken between the last check and the upload. The window is reduced,
not eliminated. crates.io versions are immutable: a lost race spends the version, and recovery is a
version bump, never a retry.

### Cleanup safety

The gate stages into a unique `target/release-gate-*` directory it creates with `mkdtemp` and owns.
It refuses a `CARGO_TARGET_DIR` (or a `target/`) that traverses a symbolic link, and every removal
checks that the path is a real child of the directory this run created. It never deletes a fixed
`release-gate` name that another checkout could have placed behind a symlink.

`cargo package -p neotales-config-keepass` on its own still fails until the core is published.
That failure is expected and is not a gate. Use the gate, which stages the core it needs.

## First publish of a new crate

crates.io trusted publishing cannot create a brand-new crate. The OIDC exchange
needs a crate that exists and a trusted publisher configured on it, so a cold
start over OIDC is rejected by crates.io. Do not try to build one.

The first version of a new crate is `0.0.0-alpha.0`. Not `0.0.0-alpha.1` and
not `0.1.0`. The first upload is manual, and a manual upload needs a version
nothing else has claimed.

The first upload goes through `/home/dev/work/rs/publish.sh <dir>`. That script
lives beside this folder, in `~/work/rs`, and is deliberately not copied into
the crate repository: it is how the first version of every crate is uploaded,
not part of any one crate.

1. Set `version = "0.0.0-alpha.0"` in every member manifest that is being
   published together, commit it, and leave the working tree clean. Do not
   publish from a tree with uncommitted changes.
2. Run the gate and the preflight before uploading anything:

   ```sh
   python3 eng/release/stage_gate.py gate
   python3 eng/release/stage_gate.py preflight --artifact-dir target/package
   ```

   The gate stages the candidate core locally, verifies the provider against
   it, and uploads nothing. Preflight then requires both versions to be absent
   (or already present at the gate's exact checksum); a version spent at other
   bytes, or a matching-but-yanked version, stops here. The two manual uploads
   are then not the first time the provider has been packaged. If the gate
   fails, the manual upload would fail at the provider with the core already
   published, which is the state this repository's release process exists to
   avoid.

   `publish.sh` does not use the exact-byte `publish` command described above,
   so it has no local proof that the tarball cargo sends equals the gate
   artifact. The tag run for the same version compares the registry checksum
   against the gate artifact and refuses if they differ. That is why a manual
   first upload is followed by a tag run, not treated as its own provenance.
3. Publish the dependency first. This repository's core is `config/`:

   ```sh
   ~/work/rs/publish.sh config
   ~/work/rs/publish.sh keepass
   ```

   The provider's upload fails if `neotales-config` 0.0.0-alpha.0 is not on
   crates.io yet: `cargo publish` verifies the package against the registry, not
   against this checkout. That is the intended order. `publish.sh` reads the
   token itself and passes it to `cargo` through the environment. Do not put a
   token on the command line, in the repository, or in shell history.
4. Add the trusted publisher on crates.io for each crate, in the crate's
   settings: owner `neotales`, repository name, workflow filename `release.yml`,
   and an empty environment. Leave the environment field empty. The `publish`
   job in `release.yml` does not set an environment, and crates.io compares the
   two fields: an environment name here that the job does not set, or the
   reverse, fails the token exchange and nothing else fails with it. If someone
   later sets an environment name on crates.io, the job must set that exact
   name.
5. Bump to the next version and let tags take over. Do not run `publish.sh`
   for these crates again.

The release run for that first version uploads nothing. Once the trusted
publisher is configured, preflight finds both versions already on crates.io at
identical bytes and `publish` skips both with a notice. If either version is
yanked, preflight refuses instead: unyank it on crates.io if that was our yank,
otherwise bump the version. Before the trusted publisher is configured, the run
fails at the token exchange and never reaches an upload. If the manual upload's
bytes differ from what the tag run builds, preflight refuses rather than
replacing them, and the version has to be bumped. Either way the tag is
provenance for the first version, not its publish trigger. Every later version
publishes from its `v*` tag.

If the crate name is taken on crates.io by someone else, stop. A rename is a
different repository setup, not a flag.

`cargo package -p neotales-config` includes every tracked file under `config/`, and
`cargo package -p neotales-config-keepass` includes every tracked file under `keepass/`, so each
crate README, its licence, its docs, and its tests ship inside its published crate. The root
`RELEASE.md`, `.github/`, and the workspace manifest stay out of both packages: they belong to the
repository, not to a crate. The README style guide lives at `~/work/rs/README.STYLE.md` and is not
part of a crate.

## Later releases

1. Bump the version in every member manifest that shares the group version, and in the provider's
   dependency on `neotales-config`, then commit. `Cargo.lock` is committed on purpose, so run
   `cargo build` and commit the updated lockfile with it. The lockfile is what the scheduled audit
   checks.
2. Push the commit.
3. Tag it `vX.Y.Z` and push the tag.
4. The workflow runs the checks and packages the core without publishing. The publish job then
   runs the gate, asks crates.io about both members with `preflight` before the token exists,
   publishes the core only after the publish path's bytes equal the gate artifact, and then does
   the same for the provider with a token minted for that run. The order is the dependency order,
   and the verification comes before the token exists.

A manual run, `workflow_dispatch`, runs the checks, packaging and the gate, plus a preflight of
both members, and never publishes. That includes a manual run started at a `v*` tag ref: the
publish job requires a push of a `v*` tag, so the event and the ref must both be right.

After the manual first upload is done, switch each crate to Trusted Publishing
Only mode on crates.io. That rejects token publishes everywhere, so a token
cannot publish that crate later.

## When the core published and the provider did not

The gate makes this state unlikely, not impossible: a registry can fail, and an upload can be
rejected for a reason packaging cannot see. A crates.io version cannot be replaced, so the core
must not be published twice and the provider must not be versioned on its own: the version rule
requires both manifests to carry the tag's version.

Recover by re-running the publish job from the same tag:

```sh
gh run list --workflow release.yml --limit 5
gh run rerun <run-id> --failed
```

The re-run keeps the tag and the `push` event. Before uploading anything it asks crates.io about
BOTH members and compares each published checksum with the artifact it just built:

| Registry answer | What the job does |
| --- | --- |
| the version is not there | It publishes, as a normal first upload, after proving the publish path's bytes equal the gate artifact. |
| same version, same checksum as the artifact | The failed run uploaded this exact artifact. It skips that upload with a notice and continues to the next member. |
| same version, different checksum | Something else owns the version. It fails before any upload in this run. |
| same checksum, but yanked | The version is not installable and cannot be uploaded again. It fails before any upload in this run. Unyank it on crates.io if that was our yank; otherwise bump the version. |

The gate artifact is not the upload artifact unless proven: `publish` runs the publish path's dry
run first and requires the tarball cargo would send to hash to the same bytes the gate verified. If
it does not, the member is not uploaded. After an upload it requires crates.io's checksum to equal
the gate artifact. The core's proof comes before any upload; the provider's comes after the core
upload, because the publish path needs the published core to resolve. If that proof fails, the
provider is not uploaded, the core is published, and the next re-run skips the core.

Preflight runs before the token exchange and is repeated inside each `publish` before its upload.
The registry can still change between those checks and the upload. A crates.io version is
immutable, so a lost race spends that version: the job fails, and the only recovery is to bump the
patch version in every manifest and tag again. Do not retry the tag at the same version.

If the provider still fails after the core is published, the version is spent. Bump the patch
version in both manifests and in the provider's dependency, commit, and tag again. Do not finish
the group by hand with `cargo publish` and a token: that path has no gate, no preflight and no
checksum decision, and it leaves the release state in someone's shell history. Do not edit the
workflow to swallow a failed upload.

## What is not allowed

- `pull_request_target` and `workflow_run` are not used anywhere, and must not
  be added. Both can run untrusted code with publishing credentials in scope.
- No long-lived crates.io token in GitHub secrets for this workflow. The job
  reads `id-token: write` and nothing else.
- No publishing from a branch. The tag check exists so the artifact has a name.
- No publishing from `workflow_dispatch`, including at a tag ref. The publish
  job's condition is the event, not only the ref.
- No republishing a version. crates.io rejects it. If a publish fails after the
  upload was accepted, bump the patch version and release again.
- No skipping an upload without comparing the published checksum with the
  artifact this run built. A version string alone cannot tell a completed
  release from a different tarball.
- No uploading a member after another member's preflight blocked. The plan
  carries zero uploads when any member is mismatched, yanked, or inconsistent.
- No uploading bytes the publish path did not prove equal to the gate artifact.
  Hashing `target/package/` alone is not that proof.
- No deleting a target directory the gate did not create, and no following a
  symlinked target to do it.

## Failure modes

| Symptom | Cause |
| --- | --- |
| `version` job: tag does not match `vX.Y.Z` | A member manifest version and the tag disagree. Delete the tag, fix, re-tag. |
| Gate: a member does not package or verify | The crate's published form does not build, usually a dependency that is not on crates.io or a path-only dependency without a version. Nothing was uploaded. Fix the crate and release again. |
| Gate: `failed to select a version` for `neotales-config`, or `the lock file needs to be updated` | The provider's dependency version and the core manifest disagree, or the version bump did not update `Cargo.lock`. Nothing was uploaded. Fix the versions, run `cargo build`, commit the lockfile, and tag again. |
| Gate: `no matching package named neotales-config` | The staged registry lost the core. That is a bug in the gate, not a release problem, and it is the state this gate exists to make impossible. The CI `release-gate` job injects this failure on every run. |
| Gate: `cannot read https://index.crates.io/...` | The index was unreachable while staging the locked dependencies. Nothing was uploaded; re-run the job. |
| Gate: `... is a symbolic link` | `CARGO_TARGET_DIR` or `target/` traverses a symlink. The gate refuses to create or delete through it. Point the target at a real directory. Nothing was uploaded. |
| Publish job: OIDC exchange fails | The crate does not exist yet, or the trusted publisher does not match: wrong repository, wrong workflow filename, or an environment name that is set on one side and not the other. |
| Preflight: `different` checksum | crates.io already holds this version at other bytes. It fails before any upload. A version cannot be replaced: bump the patch version and release a new tag. Do not retry the tag. |
| Preflight: `yanked` | crates.io holds this exact artifact but the version is yanked. It fails before any upload. Unyank it on crates.io if that was our yank; otherwise bump the version. A yanked version cannot be uploaded again. |
| Preflight: provider exists while the core does not | The registry state is inconsistent with a group release. Nothing was uploaded. Do not publish around it; investigate the crate's version history and bump the group version. |
| Publish: `the publish path packaged ... but the gate verified ...` | Cargo's publish path does not produce the bytes the gate verified. Nothing was uploaded for that member. Do not publish by hand; fix the nondeterminism or bump the version and tag again. |
| Publish: core skipped with `already published` | A previous run of the same job uploaded this exact artifact. This is the recovery path; the provider step continues. |
| Publish: `cargo publish` exited N, but crates.io holds the exact bytes | The registry accepted the upload and cargo failed later, usually its own confirmation wait. Investigate the cargo error, but do not retry the upload at that version. |
| Publish: `cargo publish` rejects the token | Trusted Publishing Only mode is on and the crate has no trusted publisher configured. |
| Publish: `did not report ... within N seconds` | The upload may be queued or may not have been accepted. Do not publish by hand: re-run the job. Preflight will report absent (retry) or identical (skip). |
| Publish: `cannot read https://index.crates.io/...` after the upload | The registry was unreachable while confirming the checksum. The upload may have been accepted. Re-run the job; preflight will report absent (retry) or identical (skip), and will block if the bytes differ. |
| `cargo package -p neotales-config-keepass` fails locally | The core version is not on crates.io, and Cargo resolves the path dependency from the registry while preparing the package, even with `--no-verify`. Run `python3 eng/release/stage_gate.py gate` instead; it stages the core and verifies the provider. |
