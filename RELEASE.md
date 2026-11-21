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
that would be uploaded, against the members before it. What it does not prove: crate-name
ownership, the trusted publisher configuration, or that crates.io will accept the upload. Those
remain the publish job's failures, and the checksum decision below is what makes them recoverable.

The gate also leaves both `.crate` files in `target/package/`. The publish job's decisions use
those files, never a version string: it asks the registry what bytes it holds for the version and
compares them with the artifact this run built.

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
2. Run the gate before uploading anything:

   ```sh
   python3 eng/release/stage_gate.py gate
   ```

   It stages the candidate core locally, verifies the provider against it, and
   uploads nothing. The two manual uploads are then not the first time the
   provider has been packaged. If the gate fails, the manual upload would fail
   at the provider with the core already published, which is the state this
   repository's release process exists to avoid.
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
publisher is configured, the checksum decision finds both versions already on
crates.io at identical bytes and skips both uploads with a notice. Before it is
configured, the run fails at the token exchange and never reaches an upload. If
the manual upload's bytes differ from what the tag run builds, the decision
refuses rather than replacing them, and the version has to be bumped. Either
way the tag is provenance for the first version, not its publish trigger. Every
later version publishes from its `v*` tag.

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
   verifies the provider against the staged core, publishes the core, and publishes the provider
   with a token minted for that run. The order is the dependency order, and the verification comes
   before the token exists.

A manual run, `workflow_dispatch`, runs the checks, packaging and the gate, and
never publishes. That includes a manual run started at a `v*` tag ref: the
publish job requires a push of a `v*` tag, so the event and the ref must both be
right.

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

The re-run keeps the tag and the `push` event. Before uploading the core it asks crates.io for that
version and compares the published checksum with the artifact it just built:

| Registry answer | What the job does |
| --- | --- |
| the version is not there | It publishes, as a normal first upload. |
| same version, same checksum as the artifact | The failed run uploaded this exact artifact. It skips the upload with a notice and continues to the provider. |
| same version, different checksum | Something else owns the version. It fails without publishing anything. |

The provider step makes the same decision against its own artifact, so re-running a job after a
complete release publishes nothing and succeeds. This is the only automatic skip in the release,
and it is bounded by a checksum: it never skips a version whose published bytes differ from the
artifact this run built.

If the provider still fails after the core is published, the version is spent. Bump the patch
version in both manifests and in the provider's dependency, commit, and tag again. Do not finish
the group by hand with `cargo publish` and a token: that path has no gate and no checksum decision,
and it leaves the release state in someone's shell history. Do not edit the workflow to swallow a
failed upload.

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

## Failure modes

| Symptom | Cause |
| --- | --- |
| `version` job: tag does not match `vX.Y.Z` | A member manifest version and the tag disagree. Delete the tag, fix, re-tag. |
| Gate: a member does not package or verify | The crate's published form does not build, usually a dependency that is not on crates.io or a path-only dependency without a version. Nothing was uploaded. Fix the crate and release again. |
| Gate: `failed to select a version` for `neotales-config`, or `the lock file needs to be updated` | The provider's dependency version and the core manifest disagree, or the version bump did not update `Cargo.lock`. Nothing was uploaded. Fix the versions, run `cargo build`, commit the lockfile, and tag again. |
| Gate: `no matching package named neotales-config` | The staged registry lost the core. That is a bug in the gate, not a release problem, and it is the state this gate exists to make impossible. The CI `release-gate` job injects this failure on every run. |
| Gate: `cannot read https://index.crates.io/...` | The index was unreachable while staging the locked dependencies. Nothing was uploaded; re-run the job. |
| Publish job: OIDC exchange fails | The crate does not exist yet, or the trusted publisher does not match: wrong repository, wrong workflow filename, or an environment name that is set on one side and not the other. |
| Publish job: `different` checksum | crates.io already holds this version at other bytes. A version cannot be replaced: bump the patch version and release a new tag. Do not retry the tag. |
| Publish job: core skipped with `already published` | A previous run of the same job uploaded this exact artifact. This is the recovery path; the provider step continues. |
| Publish job: `cargo publish` rejects the token | Trusted Publishing Only mode is on and the crate has no trusted publisher configured. |
| `cargo package -p neotales-config-keepass` fails locally | The core version is not on crates.io, and Cargo resolves the path dependency from the registry while preparing the package, even with `--no-verify`. Run `python3 eng/release/stage_gate.py gate` instead; it stages the core and verifies the provider. |
