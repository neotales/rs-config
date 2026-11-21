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
cargo publish -p neotales-config --dry-run
git tag v0.3.1
git push origin dev
git push origin v0.3.1
```

The one exception is a new crate's first version, which is uploaded by hand and
never from a tag. That is the next section.

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
2. Publish the dependency first. This repository's core is `config/`:

   ```sh
   ~/work/rs/publish.sh config
   ~/work/rs/publish.sh keepass
   ```

   The provider's upload fails if `neotales-config` 0.0.0-alpha.0 is not on
   crates.io yet: `cargo publish` verifies the package against the registry, not
   against this checkout. That is the intended order. `publish.sh` reads the
   token itself and passes it to `cargo` through the environment. Do not put a
   token on the command line, in the repository, or in shell history.
3. Add the trusted publisher on crates.io for each crate, in the crate's
   settings: owner `neotales`, repository name, workflow filename `release.yml`,
   and an empty environment. Leave the environment field empty. The `publish`
   job in `release.yml` does not set an environment, and crates.io compares the
   two fields: an environment name here that the job does not set, or the
   reverse, fails the token exchange and nothing else fails with it. If someone
   later sets an environment name on crates.io, the job must set that exact
   name.
4. Bump to the next version and let tags take over. Do not run `publish.sh`
   for these crates again.

The release run for that first version fails, because the version it would
publish is already uploaded. The tag is provenance for the first version, not
its publish trigger. Every later version publishes from its `v*` tag.

If the crate name is taken on crates.io by someone else, stop. A rename is a
different repository setup, not a flag.

`cargo package -p neotales-config` includes every tracked file under `config/`, and
`cargo package -p neotales-config-keepass` includes every tracked file under `keepass/`, so each
crate README, its licence, its docs, and its tests ship inside its published crate. The root
`RELEASE.md`, `.github/`, and the workspace manifest stay out of both packages: they belong to the
repository, not to a crate. The README style guide lives at `~/work/rs/README.STYLE.md` and is not
part of a crate.

A `cargo package -p neotales-config-keepass`, with or without verification, needs the core's version
on crates.io. Cargo resolves the path dependency from the registry while preparing the package, so
there is no local packaging gate for the provider before the core has been uploaded. Until then, the
gate is the workspace build and test plus the provider's `cargo publish` in the publish job, which
runs after the core upload. Do not record a failed package as a pass.

## Later releases

1. Bump the version in every member manifest that shares the group version, and in the provider's
   dependency on `neotales-config`, then commit. `Cargo.lock` is committed on purpose, so run
   `cargo build` and commit the updated lockfile with it. The lockfile is what the scheduled audit
   checks.
2. Push the commit.
3. Tag it `vX.Y.Z` and push the tag.
4. The workflow runs the checks, packages the core without publishing, then publishes the core and
   the provider with a token minted for that run. The order is the dependency order.

A manual run, `workflow_dispatch`, runs the checks and packaging only. It
never publishes, whatever branch it is started from.

After the manual first upload is done, switch each crate to Trusted Publishing
Only mode on crates.io. That rejects token publishes everywhere, so a token
cannot publish that crate later.

## What is not allowed

- `pull_request_target` and `workflow_run` are not used anywhere, and must not
  be added. Both can run untrusted code with publishing credentials in scope.
- No long-lived crates.io token in GitHub secrets for this workflow. The job
  reads `id-token: write` and nothing else.
- No publishing from a branch. The tag check exists so the artifact has a name.
- No republishing a version. crates.io rejects it. If a publish fails after the
  upload was accepted, bump the patch version and release again.

## Failure modes

| Symptom | Cause |
| --- | --- |
| `version` job: tag does not match `vX.Y.Z` | A member manifest version and the tag disagree. Delete the tag, fix, re-tag. |
| Publish job: OIDC exchange fails | The crate does not exist yet, or the trusted publisher does not match: wrong repository, wrong workflow filename, or an environment name that is set on one side and not the other. |
| Publish job: version already uploaded | The version was published before, usually by the manual first release. Bump the version. |
| Publish job: `cargo publish` rejects the token | Trusted Publishing Only mode is on and the crate has no trusted publisher configured. |
| Provider publish: `neotales-config` version not found | The core version is not on crates.io yet. Publish the core first; this is why the two crates share a version and publish in dependency order. |
| `cargo package -p neotales-config-keepass` cannot verify | The core version is not on crates.io. Cargo resolves the path dependency from the registry while preparing the package, even with `--no-verify`, so the local state before the first upload cannot produce a package. The publish job uploads the core first. |
