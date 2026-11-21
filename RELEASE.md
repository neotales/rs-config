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
together, in dependency order, and `gate` writes one machine-readable record of
the commit, the clean flag, and each verified artifact's SHA-256 and manifest
digest. It also prints the SHA-256 of that record and can write it to a separate
path. `preflight` requires that digest as a separate input and checks the record
bytes against it before it parses the record, refuses a stale commit, a changed
manifest, a missing artifact or an artifact whose bytes no longer match, and only
then asks crates.io anything and reports the registry state of those recorded
digests. `system-admin` publishes; the published artifact is downloaded and
compared with the recorded digest.

That comparison happens after the upload. It proves what crates.io serves hashes
to the verified digest; it does not prove that `cargo publish` sent those exact
bytes, because it repackages the checkout. Nothing here claims that guarantee.

## What runs on a tag

`release.yml` runs on a push of a `v*` tag:

| Job | What it does |
| --- | --- |
| `check` | Format, clippy, tests, a `cargo publish --dry-run` packaging smoke test, and (on `workflow_dispatch` only) the gate and preflight, with the gate record's digest passed from the gate step to the preflight step. |
| `version` | Requires the tag to equal `v` plus the version in every member manifest. |
| `verify` | Runs the gate, which writes the record of each artifact's SHA-256 and manifest digest and prints the record's own SHA-256; the step captures that digest as its output. Preflight receives it separately, checks the record bytes against it before parsing, then rejects a stale commit, changed manifest, missing artifact or changed bytes before any plan; it asks crates.io (read-only) about the recorded digests, proves the tree is the tag and unmodified, and states that nothing was published. |

The `verify` job has `permissions: contents: read`. It does not request
`id-token`, and there is no `CARGO_REGISTRY_TOKEN` secret in this repository.
A green run means **the tag is verified, not released**. The job summary carries
the recorded digests and the manual plan.

A `workflow_dispatch` run never reaches `verify`; it runs the checks, the gate
and preflight, with the record's digest passed between the steps as a step
output, and publishes nothing.

`preflight` does not run `cargo package` and does not repackage the tree. It
reports on the artifacts the gate built and recorded; it does not produce a
fresh build to compare with the registry.

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
python3 eng/release/stage_gate.py gate \
  --record target/package/gate-record.json \
  --record-sha256-out target/package/gate-record.sha256
RECORD_SHA256=<paste the digest the gate printed>
python3 eng/release/stage_gate.py preflight \
  --record target/package/gate-record.json \
  --expected-record-sha256 "$RECORD_SHA256" \
  --artifact-dir target/package
git tag v0.3.1
git push origin dev
git push origin v0.3.1
```

The tag run verifies the tag. It does not publish it. The first version of a new
crate is uploaded by `system-admin` and has no tag-driven history at all; see
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
4. fails the run if any member does not package and verify;
5. writes the durable record at `--record` (default
   `target/package/gate-record.json`): the commit, whether that checkout was
   clean, and for every member the artifact filename, its SHA-256, the manifest
   path, and the manifest's SHA-256. The record is written atomically, so a
   half-written record is never mistaken for a capture. It also prints the
   record's own SHA-256 and, with `--record-sha256-out`, writes that digest to a
   separate path. Keep the record under the ignored `target/` directory or
   outside the checkout: preflight refuses a working tree with untracked
   changes, and an untracked record inside the tree counts as one.

After staging, the verification is offline. `crates.io` is replaced by the
staging directory, so resolution cannot reach the network and a verification
failure is a packaging failure, not a flake. Staging itself needs the network
once, through `cargo fetch --locked`, which is also what a cold runner needs
before it can build anything.

What the gate proves: every member packages, every member builds from the exact
`.crate` bytes the gate staged against the members before it, and the record
describes that commit, that clean checkout and those bytes. `cargo package`
stamps the commit and dirty flag into `.cargo_vcs_info.json` inside each
`.crate`; the gate refuses an artifact whose stamp is not the commit it is about
to record, and it refuses any artifact whose stamp says `dirty`. A clean tree at
record time does not clean a package: reverting or committing changes after
`cargo package` leaves those bytes stamped `dirty`, so the gate refuses them
rather than record a dirty build. What it does not prove: that crates.io will
accept the upload, that the bytes `cargo publish` repackages later are identical,
or that a published artifact matches. The first is why `system-admin` publishes;
the last two are why the recorded digest is compared with the artifact
downloaded from crates.io afterwards.

### The gate artifact is not the upload artifact

`cargo publish` repackages into its own target directory and uploads that
tarball. Nothing in this repository claims that the gate artifact is the upload
artifact. The gate artifact is the reference: `gate` records its SHA-256 once in
the record, and that same digest is what you check against the file crates.io
serves after a manual upload. This is a comparison after the fact, not a check
that bound the bytes that were sent, and the documentation says so plainly.

### The record digest is a handoff, not tamper-proofing

A record and its artifacts live in the same filesystem, so a record that agrees
with an artifact beside it proves only that the two agree. `gate` therefore
prints the SHA-256 of the record it wrote and can write that digest with
`--record-sha256-out`. `preflight` requires the digest as
`--expected-record-sha256` and hashes the record bytes before it parses them. In
CI the gate step writes the digest to a separate path and immediately captures
it as a step output; the preflight step receives that output through the
workflow's step context, not by reading a file the record and artifacts can be
rewritten with. A record and artifacts replaced together then hash to a
different record and are refused, before the JSON is read and before crates.io
is queried. The self-test exercises exactly that pair mutation.

This is a separation of inputs, not tamper-proofing. It only holds if the
expected digest really came from somewhere the workspace cannot also change: a
CI step output from the gate step, or the operator's own notes. If an attacker
can rewrite the record, the artifacts **and** the value passed to
`--expected-record-sha256`, the anchor is defeated. The digest binds the record
to the gate run; it does not make the filesystem trustworthy.

`preflight` consumes the record rather than re-deriving it. Before it asks
crates.io anything or prints a plan, it checks:

- the record's bytes hash to the digest captured when the gate wrote it, and the
  bytes parsed are exactly those bytes: the digest and the parse share one read,
  so a record swapped in between is refused rather than parsed unchecked;
- the record's commit equals `HEAD`;
- the record was written from a clean checkout, and the checkout is still clean;
- each member's manifest is the one the record names, at the recorded version and
  manifest digest;
- each artifact exists and still hashes to the digest in the record.

Any mismatch is a failure. The new hash is never adopted as the reference: a
modified artifact, a missing artifact, a stale commit or a changed manifest all
stop the run before a plan. `preflight` does not run `cargo package` and does not
repackage the tree.

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
compare afterwards. The registry is read a second time with the same recorded
digests; if it changed while the plan was built, the run fails and asks you to
re-run rather than acting on a stale plan. The record and the artifacts are
re-checked at the end too: a file that moved while the registry was queried is
not silently reported as verified.

A record failure happens before any registry query and before any plan. The
messages name what moved: the record does not hash to the captured digest, `the
gate record is stale`, `the checkout has changed since the gate ran`, `manifest
changed after the gate verified it`, `artifact changed after the gate verified
it`, or `is missing`. Re-run the gate from a clean checkout of the tag; do not
publish bytes whose digest was not recorded by that gate run. A package stamped
`dirty=true` fails at the gate, so it never reaches a record at all.

## Publishing by hand

This is the only publishing path. `system-admin` alone handles the credentials
and performs the upload: it is the only persona that reads
`CARGO_REGISTRY_TOKEN`. No other persona reads, exports, or types the token, and
it never enters this repository, the workflow, `stage_gate.py`, a command line,
or shell history.

The operator prepares the evidence and performs the download-and-compare check.
`system-admin` uploads. Do all of it from a **clean checkout of the tag**, not
from a working tree and not from a branch:

```sh
git status --porcelain          # must be empty
git describe --tags --exact-match HEAD   # must print the tag
```

1. Produce the record, with no credential. The `verify` job does this and prints
   the digests; run it locally if you need to:

   ```sh
   python3 eng/release/stage_gate.py gate \
     --record target/package/gate-record.json \
     --record-sha256-out target/package/gate-record.sha256
   # Write the digest below into your notes. It must not be re-read from the
   # workspace later, because that is where the record and artifacts can move.
   cat target/package/gate-record.sha256
   RECORD_SHA256=<paste the digest the gate printed>
   python3 eng/release/stage_gate.py preflight \
     --record target/package/gate-record.json \
     --expected-record-sha256 "$RECORD_SHA256" \
     --artifact-dir target/package
   ```

   `preflight` must exit zero. Write down the two artifact `sha256` lines and
   the record digest, or hand the record to `system-admin`. If it exits non-zero
   because of `different`, `yanked`, an inconsistent group, a stale commit, a
   changed document or a record digest mismatch, stop: the version cannot be
   published as verified.

2. Hand the **core** upload to `system-admin`. The core goes first:

   ```sh
   cargo publish --package neotales-config --locked
   ```

   `system-admin` supplies the token. The operator does not run this with a token
   in their own environment.

3. Immediately download the core and compare it with the record, **before any
   provider upload**:

   ```sh
   name=neotales-config
   version=$(python3 -c 'import tomllib;print(tomllib.load(open("config/Cargo.toml","rb"))["package"]["version"])')
   curl -fsSL -o "/tmp/$name-$version.crate" \
     "https://static.crates.io/crates/$name/$name-$version.crate"
   sha256sum "/tmp/$name-$version.crate"
   python3 eng/release/stage_gate.py state \
     --manifest config/Cargo.toml --artifact-dir /tmp
   ```

   The `sha256sum` must equal the core digest in the record, and `state` must
   print `identical` (it points at the downloaded copy, so it confirms the
   registry's checksum describes that same file).

   **If either check differs, or the download fails, STOP.** Do not upload the
   provider. Do not retry the core, and do not yank-and-republish. The version is
   spent: bump the version in every manifest, commit, tag, and start again.

4. Only after step 3 passes, hand the **provider** upload to `system-admin`:

   ```sh
   cargo publish --package neotales-config-keepass --locked
   ```

5. Download the provider and compare it with its recorded digest:

   ```sh
   name=neotales-config-keepass
   version=$(python3 -c 'import tomllib;print(tomllib.load(open("keepass/Cargo.toml","rb"))["package"]["version"])')
   curl -fsSL -o "/tmp/$name-$version.crate" \
     "https://static.crates.io/crates/$name/$name-$version.crate"
   sha256sum "/tmp/$name-$version.crate"
   python3 eng/release/stage_gate.py state \
     --manifest keepass/Cargo.toml --artifact-dir /tmp
   ```

   The provider's `sha256sum` must equal its recorded digest and `state` must
   print `identical`. If it does not, stop: do not retry the version.

6. Confirm the end state. Both downloaded `sha256sum` values must equal their
   recorded digests, and both `state` calls must print `identical`. If you also
   want CI's view, consume the same record again:

   ```sh
   python3 eng/release/stage_gate.py preflight \
     --record target/package/gate-record.json \
     --expected-record-sha256 "$RECORD_SHA256" \
     --artifact-dir target/package
   ```

   `preflight` reads the recorded artifacts and does not repackage, so it does
   not compare a fresh build with the registry. Both members must report
   `identical` and the plan must say `nothing to publish`. Use the same record
   digest you captured in step 1; re-reading it from the workspace would defeat
   the check.

The download comparison happens after the upload. It proves that the bytes
crates.io serves hash to the recorded digest; it does not prove that the bytes
`cargo publish` sent were those bytes. That guarantee is not available locally,
and nothing here claims it.

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
part of any one crate. `system-admin` alone runs it and handles the credential;
no other persona reads the token. It never becomes part of CI, and this
repository never holds its token.

1. Set `version = "0.0.0-alpha.0"` in every member manifest that is being
   published together, commit it, and leave the working tree clean.
2. Produce the record before uploading anything, with no credential:

   ```sh
   python3 eng/release/stage_gate.py gate \
     --record target/package/gate-record.json \
     --record-sha256-out target/package/gate-record.sha256
   RECORD_SHA256=<paste the digest the gate printed>
   python3 eng/release/stage_gate.py preflight \
     --record target/package/gate-record.json \
     --expected-record-sha256 "$RECORD_SHA256" \
     --artifact-dir target/package
   ```

   The gate stages the candidate core locally and verifies the provider against
   it, and uploads nothing. `preflight` consumes that record and requires both
   versions to be absent (or already present at the recorded digest). A version
   spent at other bytes, a matching-but-yanked version, a stale commit, a changed
   artifact or a record that no longer hashes to the digest the gate printed
   stops here. Hand the record and its digest to `system-admin`. Skipping this
   check risks discovering that the provider cannot be packaged only after the
   core is already published, which is the state this repository's gate exists
   to avoid.
3. `system-admin` uploads the **core** first. This repository's core is
   `config/`:

   ```sh
   ~/work/rs/publish.sh config
   ```

4. Immediately download the core and compare it with the record **before the
   provider is uploaded**, as in
   [Publishing by hand](#publishing-by-hand) step 3. If the download fails or the
   digest differs, **STOP**: do not upload the provider, do not retry, and do not
   yank-and-republish. The version is spent; bump it in every manifest, commit,
   tag, and start again.
5. Only after the core comparison passes, `system-admin` uploads the provider:

   ```sh
   ~/work/rs/publish.sh keepass
   ```

   The provider's upload fails if `neotales-config` 0.0.0-alpha.0 is not on
   crates.io yet: `cargo publish` verifies the package against the registry, not
   against this checkout. That is the intended order.
6. Download the provider and compare it with its recorded digest, as in
   [Publishing by hand](#publishing-by-hand) step 4. `publish.sh` does not
   compare anything; the download-and-compare is what tells you the served bytes
   match the record. It does not prove which bytes `publish.sh` sent.

If the crate name is taken on crates.io by someone else, stop. A rename is a
different repository setup, not a flag.

**Do not enable Trusted Publishing Only for these crates.** The only publisher is
`system-admin` with a token. A trusted publisher configured earlier is unused by
`release.yml`, which no longer requests a token; enabling the restriction would
break manual publishing without replacing it with anything proven.

Each package excludes its `benches/` directory, so
`cargo package -p neotales-config` includes every tracked file under `config/`
except that directory, and `cargo package -p neotales-config-keepass` does the
same under `keepass/`. Each crate README, its licence, its docs, and its tests
still ship inside its published crate; the benchmark sources and their generated
fixtures do not. The root `RELEASE.md`, `.github/`, and the workspace
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
4. The workflow runs the checks and the gate; the gate writes the record and
   prints its SHA-256, which the job captures as a step output; preflight
   receives that digest separately, refuses a stale commit, a changed manifest,
   artifact or record, reports the registry state and stops. Nothing is
   published.
5. Hand the record and its digest to `system-admin`, who uploads from a clean
   checkout of the tag in dependency order. After the **core** upload, download
   it and compare its digest with the record **before** the provider is
   uploaded; stop on a mismatch. Then upload the provider and compare it the
   same way.

## When the core published and the provider did not

The group can be left half-published by a manual error, a registry failure, or a
token that expired between the two uploads. A crates.io version cannot be
replaced, so the core must not be published twice and the provider must not be
versioned on its own: the version rule requires both manifests to carry the
tag's version.

Ask the registry first, consuming the record from the tag. If you do not have a
record from a clean checkout of that tag, produce one before asking:

```sh
python3 eng/release/stage_gate.py gate \
  --record target/package/gate-record.json \
  --record-sha256-out target/package/gate-record.sha256
# Capture this digest somewhere the record and artifacts cannot be rewritten.
# In CI it is the gate step's output; locally, write it into your notes.
RECORD_SHA256=<paste the digest the gate printed>
python3 eng/release/stage_gate.py preflight \
  --record target/package/gate-record.json \
  --expected-record-sha256 "$RECORD_SHA256" \
  --artifact-dir target/package
```

| Registry answer for the recorded digest | What to do |
| --- | --- |
| the core is `identical`, the provider is `absent` | This is the expected half-published state. Check out the same tag cleanly, confirm the core's downloaded digest matches the record, then hand the provider upload to `system-admin`. Download the provider and compare it with the record. Do not re-publish the core. |
| both are `identical` | The release is complete. Nothing to do. |
| any member is `different` | That version is spent. Bump the version in every manifest and tag again. Do not retry, and do not try to replace the version. |
| a member is `yanked` with a matching digest | The version is not installable and cannot be published again. Unyank it on crates.io if that was our yank; otherwise bump the version. |
| the provider is published while the core is absent | The registry state is inconsistent with a group release. Investigate the crate's version history and bump the group version. Do not publish around it. |

Re-running the workflow from the tag re-verifies and re-reports. It cannot
publish and cannot change the state above. If the record is stale, a manifest
changed, an artifact is missing or an artifact's bytes moved, preflight fails
before it prints this table; re-run the gate from the clean tag checkout.

## What is not allowed

- No automated publishing. `release.yml` must not gain an upload step, a
  registry token, or `id-token: write`, and `stage_gate.py` must not gain a
  publishing command or a token read. The `release-gate` job in `ci.yml` checks
  that the program has neither.
- `pull_request_target` and `workflow_run` are not used anywhere, and must not be
  added. Both can run untrusted code with publishing credentials in scope.
- No publishing from a branch or a dirty working tree. The tag and a clean
  checkout are what make the recorded digests describe something nameable.
  `gate` records the commit and clean flag; `preflight` refuses a record that no
  longer matches either.
- No republishing a version. crates.io rejects it. If a publish fails after the
  upload was accepted, bump the version; never retry the same version.
- No re-hashing a changed artifact and treating the new hash as the reference.
  A modified, missing or unreviewed artifact fails preflight. The gate record is
  the only authority for the digest.
- No re-reading the record digest from the workspace at preflight time. It must
  come from the gate step's output or the operator's notes; reading the `.sha256`
  file beside the record would let a rewritten record choose its own reference.
- No recording a package stamped `dirty=true`. A clean tree at record time does
  not clean the bytes `cargo package` produced; the gate refuses them.
- No claim that a manual upload matched without comparing the artifact
  downloaded from crates.io with the recorded digest. A version string alone
  cannot tell a completed release from a different tarball, and the comparison is
  after the fact: it does not prove which bytes `cargo publish` sent.
- No provider upload before the core has been downloaded and its digest compared
  with the record. A core mismatch stops the release with the provider
  unpublished.
- No credential outside `system-admin`. The operator does not read, export or
  type `CARGO_REGISTRY_TOKEN`, and it never enters the repository, the workflow,
  a command line or shell history.
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
that another checkout could have placed behind a symlink. The gate record is
written with a temporary file and an atomic rename, so a failed write cannot
leave a partial capture. `preflight` is read-only: it writes no artifact, runs no
packaging command, and there is no separate publish work directory to leak
because there is no publish step.

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
| Gate: `was packaged from commit ..., but ... is at ...` | HEAD moved while the gate was packaging, or the artifact came from another commit. Nothing was published; re-run the gate on a commit that is not moving. |
| Gate: `was packaged from a dirty tree` | `cargo package` stamped `dirty=true` into the `.crate`. Committing or reverting the changes afterwards does not change those bytes, so the gate refuses to record them. Commit or discard the changes, then run the gate again. |
| Gate: `has no .../.cargo_vcs_info.json` | `cargo package` did not stamp the commit, usually because the run is not in a Git checkout. The record cannot bind the bytes to a commit; run the gate from the real release checkout. |
| Verify: `the checkout has changes` or `HEAD is ... not ...` | The tag job is not running on a clean checkout of that exact tag. The recorded digests would describe something else. Nothing was published; investigate the checkout, not the release. |
| Preflight: `... hashes to ..., not the digest ... captured when the gate wrote it` | The record changed after the gate wrote it, or the wrong digest was supplied. Preflight refuses before parsing the record. Nothing was published; re-run the gate and pass the digest that run's gate step printed (CI: `steps.gate.outputs.record_sha256`; locally: your notes). |
| Preflight: `different` | crates.io already holds this version at other bytes. It exits non-zero. A version cannot be replaced: bump the patch version and tag again. Do not retry the tag. |
| Preflight: `yanked` | crates.io holds this exact artifact but the version is yanked. It exits non-zero. Unyank it on crates.io if that was our yank; otherwise bump the version. A yanked version cannot be published again. |
| Preflight: provider exists while the core does not | The registry state is inconsistent with a group release. Nothing was published. Do not publish around it; investigate the versions and bump the group version. |
| Preflight: `... is missing. The gate recorded ...` | The record names an artifact that `--artifact-dir` does not contain. Point preflight at the directory the gate wrote, or re-run the gate from the clean tag checkout. Do not publish from a missing artifact. |
| Preflight: `... hashes to ..., but the gate recorded ...` | An artifact changed after the gate wrote the record. The recorded digest no longer describes that file. Preflight refuses it rather than adopting the new hash. Re-run the gate from a clean tag checkout. |
| Preflight: `manifest changed after the gate verified it` | A member manifest differs from the one the gate recorded. The artifact does not describe this tree. Re-run the gate from the checkout being released. |
| Preflight: `the gate record is stale` | The record's commit is not `HEAD`. The record came from another commit. Re-run the gate on the commit being released; do not reuse the old record. |
| Preflight: `the gate record was written from a dirty checkout` | The gate ran with uncommitted changes. Its record cannot describe a tag. Commit or discard the changes and re-run the gate. |
| Preflight: `the checkout has changed since the gate ran` | The working tree moved after the record was written. Commit or discard the changes and re-run the gate. |
| Preflight: `... is missing. preflight consumes the gate record` | No record was passed, or the path is wrong. Run the gate with `--record` and pass that path to preflight. |
| Preflight: `the gate record ... changed while preflight was running` | The record was rewritten during the run. Nothing was published; re-run the gate and preflight. |
| Preflight: `crates.io changed while preflight was running` | The registry state moved between two reads. Nothing was published. Re-run the workflow for a consistent plan. |
| `verify` is green but the crate is not on crates.io | This is the intended state: the tag is verified, not released. Hand the record to `system-admin`, then compare the downloaded artifact with the recorded digest. |
| Manual publish: `cargo publish` rejects the token | The token is missing or invalid, or the crate is in Trusted Publishing Only mode. `system-admin` alone handles the token. Do not enable that mode while publishing by hand; ask `system-admin` for a token that can publish. |
| Manual publish: `cargo publish` reports an error but the artifact downloads and matches | crates.io accepted the upload and cargo failed later, usually its own confirmation wait. Investigate the cargo error; do not upload the version again. |
| Manual publish: the core's downloaded digest differs from the record | The version is spent. **Do not upload the provider**, do not retry, and do not yank-and-republish. Bump the version in every manifest and tag again. |
| Manual publish: the provider's downloaded digest differs from the record | The version is spent. Do not retry and do not yank-and-republish. Bump the version in every manifest and tag again. |
| Manual publish: `cargo publish` fails for the provider because the core is missing | The core was not published, or its version does not match the provider's dependency. Publish the core first, from the same tag, and compare its downloaded digest before the provider. |
| `cargo package -p neotales-config-keepass` fails locally | The core version is not on crates.io, and Cargo resolves the path dependency from the registry while preparing the package, even with `--no-verify`. Run `python3 eng/release/stage_gate.py gate` instead; it stages the core and verifies the provider. |
