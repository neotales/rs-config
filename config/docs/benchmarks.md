# Benchmarks

This page describes a reproducible local benchmark suite for the file providers and the core
merge. The numbers below are one machine's observations, taken while other work was running on the
same host. They are not a performance guarantee, a cross-machine comparison, or a claim that one
format's parser is better than another's.

The suite lives in `config/benches/` and `keepass/benches/`; the raw Criterion output from the
recorded runs is in `eng/benchmarks/2026-11-21-x86_64-linux/`.

## What is measured

Every file provider is measured through the same six phases. A dash means the phase is not
reachable without adding public API.

| Phase | What runs | Why it is separate |
| --- | --- | --- |
| `read` | `std::fs::read` plus `String::from_utf8` | The I/O half of `Source::load`, matching the crate's private `read_text` helper. |
| `parse` | The parser crate's own public API on text that is already in memory | Isolates the dialect parser from the read and from the provider's conversion. Not present for INI. |
| `load` | The published `Source::load` | Read, decode, parse, exact-number handling where the format has it, and conversion into `Value`. |
| `build` | `Builder::build` with the file source and a small in-memory overlay | Adds the recursive merge and per-node provenance to `load`. |
| `refresh` | `Reload::refresh` on the unchanged fixture | Captures the watched-file fingerprint, reloads every source, merges, and compares the result with the published snapshot. The fixture does not change, so the outcome is `Unchanged`: this measures reload, merge, and compare, and publishes nothing. Changed-snapshot publication is not measured. Requires `reload`. |
| `refresh-unchanged` | `Reload::refresh_if_changed` on an unchanged file | The polling path: fingerprints the watched file, compares it with the previous baseline, and skips the load. Returns `Unchanged`. Requires `reload`. |

The in-memory groups are the merge baseline:

| Benchmark | What runs |
| --- | --- |
| `memory/small/build` | One memory source holding the small model; clone, merge, provenance. |
| `memory/realistic/build` | One memory source holding the realistic model. |
| `memory/realistic-merge/build` | Two memory sources, so the map-merge path is measured with an overlay. |

`Builder::memory` clones its value on every load, so the memory baseline includes that clone. It is
not a parse benchmark and it is not a zero-copy merge benchmark.

## Feature gates and dependency impact

The library's feature set is unchanged. No benchmark feature was added and no default feature was
turned on. The benchmark target is compiled by Cargo as usual, but each provider group is behind
the same `#[cfg(feature = ...)]` as the provider itself:

- `cargo bench -p neotales-config --bench sources` runs only the memory groups.
- `cargo bench -p neotales-config --all-features --bench sources` runs every format and the reload
  groups.
- `cargo bench -p neotales-config --features json,toml --bench sources` runs those two providers.
- `cargo bench -p neotales-config-keepass --bench keepass` runs load and build;
  `--all-features` adds the reload groups.

Criterion 0.8.2 is a dev-dependency with default features off: no `plotters`, no `rayon`, and no
gnuplot. It does not appear in the normal dependency graph:

```console
$ cargo tree -p neotales-config --no-default-features -e normal
neotales-config v0.0.0-alpha.0 (/home/dev/work/rs/config/config)
└── serde v1.0.229
    └── serde_core v1.0.229
```

`serde_json` is also a dev-dependency, used only as a generic value target for the JSON and JSON5
parser-only baselines. It does not make the library's optional `json` dependency part of a default
build.

The benchmark sources are development tooling, so both package manifests exclude `benches/`:

```console
$ cargo package -p neotales-config --list
...
tests/typed.rs
$ cargo package -p neotales-config --allow-dirty
warning: ignoring benchmark `sources` as `benches/sources.rs` is not included in the published package
    Packaged 52 files, 524.6KiB (118.8KiB compressed)
```

Cargo omits a target whose source is excluded; the generated package manifest therefore has no
`[[bench]]` entry. The warning is expected. The `.crate` does not contain the benchmark, its support
module, or a KDBX fixture.

## Fixtures

`config/benches/support/mod.rs` renders the same service rows and logical field values into every
dialect, where the dialect can express them, from fixed integer formulas. There is no randomness, no
clock input, and no file outside the temporary directory. Every value is synthetic and non-secret:
loopback hosts, `fixture`-style names, and a database URL with a user but no password.

| Size | Services | JSON | JSON5 | JSONC | TOML | YAML | INI | dotenv |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `small` | 1 | 672 B | 685 B | 757 B | 648 B | 676 B | 618 B | 926 B |
| `realistic` | 96 | 25,634 B | 23,557 B | 27,999 B | 23,615 B | 26,303 B | 22,255 B | 45,363 B |

The documents are generated from the same logical model: a small root (`root_name`, `service`,
`http`, `database`, `logging`, `ratio`, `large`) and one or 96 service rows with twelve fields each.
They are not identical documents. The dialects differ in what they can express, so the cross-format
figures compare work on similarly sized documents, not the same input:

- INI values are always strings, and a section name such as `[http.timeouts]` is one literal key,
  not a nested map. The INI document therefore exercises the same line volume through a flatter
  value shape.
- dotenv values are always strings and are read through `DotenvFile::prefix("APP_")` with `.` as the
  key separator. The document has no `${...}` references; the expansion pass still runs. INI and
  dotenv cannot express the service `targets` array at all.
- JSON5 uses unquoted names, single quotes, hexadecimal ports, and a leading-decimal ratio. JSONC
  adds comments only. The logical values match JSON.
- TOML uses `[services.service_00]` tables and a `large` value at `i64::MAX`. YAML uses the YAML 1.2
  core schema and nested block mappings.

The shared formulas keep fixture differences from being the main variable, but they do not make the
fixtures identical. A format's line or byte volume, its value shape, and the conversion it needs
still differ, and those differences are part of what each row measures.

`keepass/benches/support/mod.rs` generates KDBX 4.1 fixtures in a temporary directory:

| Vault | Entries | Fields per entry | Mapped fields | AES-KDF rounds | Compression |
| --- | --- | --- | --- | --- | --- |
| `small` | 1 | 9 | 6 | 8 | none |
| `realistic` | 48 | 9 | 6 (288 mappings) | 8 | gzip |
| `realistic-kdf-600000` | 48 | 9 | 6 (288 mappings) | 600,000 | gzip |

The temporary directory is the system temp directory, which on the recorded host is `/tmp` on
tmpfs. Set `TMPDIR` to a disk-backed path to measure a different filesystem. Every benchmark process
regenerates its own fixtures, so two runs cannot share a stale file.

## Running

From the repository root, with Rust 1.100.0 installed by `rust-toolchain.toml`:

```sh
# Core file providers and the in-memory baseline, full suite.
cargo bench -p neotales-config --all-features --bench sources -- \
  --sample-size 20 --warm-up-time 0.3 --measurement-time 0.8

# KeePass provider, full suite (including the 600,000-round vault).
cargo bench -p neotales-config-keepass --all-features --bench keepass -- \
  --sample-size 20 --warm-up-time 0.3 --measurement-time 0.8
```

`--bench <name>` is required when passing Criterion flags: without it, `cargo bench` also runs the
library's unittest harness, which does not understand `--sample-size`. The suite embeds the same
20-sample / 300 ms warm-up / 800 ms measurement defaults, so a plain invocation without flags is also
bounded.

Narrow a run with a Criterion filter:

```sh
cargo bench -p neotales-config --all-features --bench sources -- 'json/realistic'
cargo bench -p neotales-config-keepass --all-features --bench keepass -- 'realistic-kdf'
```

For a change evaluation, use a longer measurement and repeat the run:

```sh
cargo bench -p neotales-config --all-features --bench sources -- \
  --sample-size 50 --warm-up-time 1 --measurement-time 3
```

Criterion writes its machine-readable samples and reports under `target/criterion/`. The default
features are off, so no plots are produced and gnuplot is not needed. Removing `target/criterion/`
before a run removes the `change:` comparison against earlier data.

## Recorded environment

| | |
| --- | --- |
| Date | 2026-11-21 (local UTC−06:00) |
| Kernel | Linux 7.2.15-1-omarchy x86_64 |
| Distribution | Omarchy (Arch Linux), BUILD_ID 4.5.0 |
| CPU | Intel Core i7-9750H, 6 cores / 12 threads, 2.60 GHz base |
| CPU frequency control | `powersave` governor; `lscpu` reported 90% scaling when captured |
| Memory | 62 GiB |
| Fixture filesystem | `/tmp` on tmpfs (32 GiB) |
| Toolchain | `rustc 1.100.0 (0ef1bc386 2026-11-09)`, `cargo 1.100.0 (3acc79c0c 2026-11-05)` |
| Profile | Cargo's `bench` profile (release-derived); `CARGO_INCREMENTAL=0` |
| Affinity | `taskset -c 2` |
| Other load | Shared machine: load average readings of 9.6–13.1 on 12 logical CPUs during the session |

Three independent runs of each suite were taken. Run 2 overlapped a period of heavy activity from
other sessions on the same host and is included only in the variability section; its multi-fold
outliers are not a property of the code. Runs 1 and 3 were used for the primary observation, and
run 3 is tabulated for the non-reload groups because its within-run ordering was the most
consistent. Even between runs 1 and 3, several small-operation groups moved by 20–80%, and one
`jsonc` group in run 1 was about five times slower than run 3.

A fourth run of each suite was taken after the `RefreshOutcome::Unchanged` assertions were added to
the timed reload closures. Only the reload groups changed in code, so those tables use run 4. Run
4's other groups are in the raw log but are not used for the other tables.

## Observed results

These are Criterion's typical estimates for the run: the slope of its per-iteration fit when it has
one, otherwise the mean. The bracketed values are the 95% confidence interval Criterion prints for
that same estimate, so when a slope is available they are not a confidence interval for the mean.
The interval describes the estimate under the run's conditions, not the spread between runs. Times
below 100 µs are printed with two decimals and should be read as approximate; the process and timer
overhead is a meaningful fraction of such a measurement.

### Small size (one service)

| Provider | `read` | `parse` | `load` | `build` |
| --- | --- | --- | --- | --- |
| JSON | 3.79 µs [3.76–3.84] | 4.09 µs [3.99–4.24] | 10.0 µs [9.8–10.3] | 24.1 µs [23.8–24.5] |
| JSON5 | 4.27 µs [4.00–4.61] | 9.52 µs [9.34–9.75] | 15.7 µs [15.5–16.1] | 29.9 µs [29.6–30.3] |
| JSONC | 3.79 µs [3.77–3.80] | 5.63 µs [5.56–5.73] | 14.3 µs [14.2–14.5] | 27.7 µs [27.5–27.9] |
| TOML | 3.83 µs [3.81–3.86] | 14.9 µs [14.7–15.2] | 24.6 µs [24.3–24.8] | 42.8 µs [41.6–43.9] |
| YAML | 3.77 µs [3.75–3.80] | 24.8 µs [24.6–25.2] | 30.6 µs [30.3–30.9] | 46.4 µs [46.0–46.8] |
| INI | 3.83 µs [3.77–3.93] | — | 9.61 µs [9.45–9.81] | 21.9 µs [21.5–22.3] |
| dotenv | 3.88 µs [3.82–3.94] | 8.51 µs [8.47–8.58] | 32.9 µs [32.2–33.8] | 51.3 µs [47.5–56.2] |

### Realistic size (96 services)

| Provider | `read` | `parse` | `load` | `build` |
| --- | --- | --- | --- | --- |
| JSON | 5.70 µs [5.64–5.79] | 199 µs [196–201] | 285 µs [279–292] | 881 µs [869–900] |
| JSON5 | 5.42 µs [5.36–5.50] | 399 µs [389–409] | 474 µs [469–479] | 1.08 ms [1.07–1.09] |
| JSONC | 5.71 µs [5.69–5.74] | 220 µs [215–229] | 457 µs [449–465] | 1.08 ms [1.06–1.11] |
| TOML | 5.37 µs [5.33–5.42] | 627 µs [620–635] | 840 µs [825–851] | 1.57 ms [1.51–1.60] |
| YAML | 5.53 µs [5.50–5.56] | 892 µs [885–900] | 1.06 ms [1.05–1.07] | 1.69 ms [1.68–1.70] |
| INI | 5.45 µs [5.41–5.50] | — | 363 µs [361–365] | 838 µs [828–848] |
| dotenv | 7.45 µs [7.35–7.55] | 406 µs [402–412] | 1.51 ms [1.48–1.54] | 2.10 ms [2.07–2.16] |

For the realistic documents, the read of a 22–45 KiB file from tmpfs is a few microseconds;
parsing and building the value tree dominate. For the JSON family, `parse` is the parser crate's generic value, while `load` additionally
scans the document for exact number tokens and copies every key and string into `Value`. JSONC's
`parse` baseline stops at `jsonc-parser`'s own tree, so the gap to `load` is larger there. INI has no
public parser-only baseline, so its `load` includes the private parse; the difference is not split
further into read and parse. dotenv's `parse` baseline is the assignment list only, without `${...}` expansion, key
mapping, or the process-environment lookup that `load` performs.

The in-memory baseline for the same realistic model is 792 µs [787–795] for one source and 827 µs
[809–854] for two sources. That is the merge and provenance cost with no parser or file read, and it
is why a file `build` is roughly its `load` plus several hundred microseconds on this host.

### Reload

Small size (run 4, post-assertion):

| Provider | `refresh` | `refresh-unchanged` |
| --- | --- | --- |
| JSON | 29.0 µs [28.4–29.9] | 4.05 µs [4.03–4.08] |
| JSON5 | 34.6 µs [34.1–35.0] | 4.15 µs [4.11–4.20] |
| JSONC | 33.2 µs [32.9–33.6] | 4.25 µs [4.22–4.28] |
| TOML | 45.5 µs [44.2–47.3] | 4.05 µs [4.02–4.09] |
| YAML | 51.9 µs [51.2–52.9] | 4.15 µs [4.13–4.18] |
| INI | 27.5 µs [27.2–27.8] | 4.08 µs [4.04–4.12] |
| dotenv | 55.0 µs [53.9–56.2] | 4.49 µs [4.43–4.56] |

Realistic size (run 4, post-assertion):

| Provider | `refresh` | `refresh-unchanged` |
| --- | --- | --- |
| JSON | 980 µs [971–990] | 31.4 µs [31.2–31.6] |
| JSON5 | 1.17 ms [1.16–1.18] | 29.0 µs [28.9–29.2] |
| JSONC | 1.20 ms [1.19–1.22] | 34.5 µs [34.2–34.7] |
| TOML | 1.71 ms [1.62–1.87] | 29.1 µs [28.9–29.4] |
| YAML | 1.82 ms [1.80–1.84] | 31.9 µs [31.7–32.1] |
| INI | 892 µs [889–895] | 27.5 µs [27.3–27.6] |
| dotenv | 2.25 ms [2.20–2.33] | 52.8 µs [52.6–53.0] |

Both `refresh` groups use an unchanged fixture. `Reload::refresh` fingerprints the watched file,
reloads and merges every source, then compares the candidate with the published snapshot; because
the file did not change, it returns `RefreshOutcome::Unchanged` and publishes nothing. The timed
closure asserts that outcome, so a failed refresh or a silently different snapshot fails the
benchmark instead of being recorded as a timing. The group therefore measures the unchanged-refresh
path, including the fingerprint and the load, merge, and comparison, and does **not** measure the
changed-snapshot publish path or subscriber notification. The `refresh-unchanged` group is the
polling path: it fingerprints the file, compares the fingerprint with the previous baseline, and
returns `Unchanged` without loading any source.

The full `refresh` is close to a fresh `build` plus the fingerprint and comparison, as expected.

### KeePass

The reload rows are from run 4, after the outcome assertions were added; the other rows are from
run 3 and their code paths were not changed.

| Vault | Phase | Time |
| --- | --- | --- |
| `small` (1 entry, KDF 8) | `read` | 3.82 µs [3.80–3.85] |
| `small` | `codec-load` | 71.9 µs [71.2–72.5] |
| `small` | `load` | 78.5 µs [77.7–79.6] |
| `small` | `build` | 87.6 µs [86.5–89.7] |
| `small` | `refresh` | 91.9 µs [91.1–92.9] |
| `small` | `refresh-unchanged` | 7.37 µs [7.01–7.68] |
| `realistic` (48 entries, KDF 8) | `read` | 3.94 µs [3.92–3.97] |
| `realistic` | `codec-load` | 1.05 ms [1.03–1.07] |
| `realistic` | `load` | 1.38 ms [1.37–1.40] |
| `realistic` | `build` | 1.55 ms [1.54–1.56] |
| `realistic` | `refresh` | 1.60 ms [1.57–1.62] |
| `realistic` | `refresh-unchanged` | 11.4 µs [11.3–11.4] |
| `realistic-kdf-600000` (48 entries, KDF 600,000) | `read` | 3.97 µs [3.94–4.02] |
| `realistic-kdf-600000` | `codec-load` | 12.25 ms [12.20–12.30] |
| `realistic-kdf-600000` | `load` | 12.68 ms [12.60–12.82] |
| `realistic-kdf-600000` | `build` | 12.92 ms [12.79–13.03] |
| `realistic-kdf-600000` | `refresh` | 12.83 ms [12.77–12.90] |
| `realistic-kdf-600000` | `refresh-unchanged` | 11.5 µs [11.4–11.5] |

`codec-load` is `KdbxFile::load` only: open, KDF, decrypt, decompress, and parse the KDBX XML into
the codec's document. `load` adds the provider's explicit mapping of 288 entry fields. At the
sample KDF cost the mapping is visible; at the codec's default 600,000 AES-KDF rounds, key
derivation is about 12 ms and dominates everything the provider adds.

The `refresh` group also uses the unchanged vault. It reloads and maps the vault (including key
derivation), merges, and compares the result with the published snapshot, returning
`RefreshOutcome::Unchanged` without publishing. The timed closure asserts that outcome, so a failed
or changed reload fails the benchmark rather than producing a failure timing. The
`refresh-unchanged` poll is still microseconds because it only fingerprints the file, compares the
fingerprint, and deliberately does not load the vault. The changed-vault publish path is not
measured.

## Variability

Three runs of each suite, reported typical estimates in ms, for the realistic `load` and `build`
groups:

| Provider | load run 1 | load run 2 | load run 3 | build run 1 | build run 2 | build run 3 |
| --- | --- | --- | --- | --- | --- | --- |
| JSON | 0.273 | 0.416 | 0.285 | 0.903 | 1.200 | 0.881 |
| JSON5 | 0.469 | 1.666 | 0.474 | 1.109 | 5.167 | 1.083 |
| JSONC | 2.551 | 4.297 | 0.457 | 3.876 | 6.386 | 1.077 |
| TOML | 0.865 | 6.596 | 0.840 | 1.554 | 11.222 | 1.565 |
| YAML | 1.183 | 5.812 | 1.062 | 2.504 | 8.900 | 1.689 |
| INI | 0.470 | 0.559 | 0.363 | 1.130 | 2.045 | 0.838 |
| dotenv | 1.454 | 1.779 | 1.506 | 3.861 | 2.204 | 2.102 |

| Case | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| KeePass small `load` | 0.083 | 0.083 | 0.079 |
| KeePass small `build` | 0.122 | 0.095 | 0.088 |
| KeePass realistic `load` | 1.487 | 1.375 | 1.382 |
| KeePass realistic `build` | 1.721 | 1.689 | 1.549 |
| KeePass 600,000-round `load` | 12.724 | 14.914 | 12.683 |
| KeePass 600,000-round `build` | 13.704 | 16.745 | 12.924 |

Run 2 was taken while the host was under sustained load and is included to show how large that
interference can be, not as a result to compare against. Run 1 also lost one `jsonc` group to the
same kind of interference. On this host, treat a difference below roughly 2× between two
quick runs as unresolved unless it reproduces over longer measurement and repeated runs. The
KeePass 600,000-round load moved by about 17% and its build by about 22% between run 1 and
run 2, which is consistent with that caveat even though the KDF dominates both.

## Limitations

- **The host was not isolated.** The recorded runs used `taskset -c 2` but no cgroup, real-time
  priority, CPU offline, or `isolcpus`. Other sessions on the same machine were active, and the CPU
  governor is `powersave`. The variability section is evidence that this matters.
- **The fixtures are in a memory-backed filesystem.** `/tmp` was tmpfs, so `read` measures a warm
  in-memory copy, not a cold disk, a network filesystem, or an SSD. Set `TMPDIR` to another path to
  change it.
- **Everything is single-process and synchronous.** There is no concurrent reader, no async
  runtime, and no `Watcher` thread measurement. The polling watcher has its own interval; only the
  operation it calls is measured.
- **The parser-only baselines are not the provider's exact code.** JSON5 is parsed into a generic
  `serde_json::Value`; YAML is parsed by `saphyr`'s document model while the provider drives
  `saphyr-parser` event by event; JSONC uses the provider's option set but not its conversion; TOML
  uses the crate's native value. Each parser baseline is an independent comparator in a different
  value shape, not the provider's code path. It therefore neither isolates exactly the parse stage
  the provider runs nor bounds the provider's cost from below: the two do different work, in either
  direction.
- **INI has no public parser-only baseline.** Its `load` is read + private parse + convert. The
  difference between `load` and `read` is not a pure parse measurement.
- **The dotenv `parse` baseline excludes expansion and key mapping.** `load` also walks the process
  environment for references, expands every assignment, and splits selected keys into paths.
- **The formats are not equal in shape.** INI and dotenv can only hold strings and cannot express the
  service arrays; INI's dotted section names are literal keys. Cross-format numbers compare document
  volume and provider work, not language expressiveness or parser quality.
- **The memory baseline clones.** `Builder::memory` calls `value.clone()` on every load, so the
  baseline is clone + merge + provenance, not a zero-copy merge.
- **KeePass KDF cost is a policy choice.** The 600,000-round fixture is the codec's AES-KDF default,
  not a statement about a real vault. KeePassXC commonly defaults to Argon2id with a memory cost;
  that path was not measured because its cost is much larger and depends on the machine and the
  vault's settings. A real vault's KDF dominates even more than the AES fixture shows.
- **No HTTP, environment, arguments, or typed-read benchmarks.** `Environment`, `Arguments`, the
  typed `get` path, and the watcher thread are out of this suite. The in-memory baseline isolates
  the merge, not a deserialization target.
- **Criterion's confidence interval is within a run and belongs to the typical estimate.** It is the
  interval for the slope when Criterion has one, otherwise for the mean. It does not include the
  between-run variability shown above. Do not read a non-overlapping interval in one run as a
  significant difference on this host.

## KeePass fixture creation without a secret

The KeePass fixtures are generated by the published codec from synthetic literals, in the same way
`keepass/tests/support/mod.rs` generates its test vaults:

- The master password is the fixed fixture literal
  `fixture-bench-master-SENTINEL`, not a user credential and not read from a store, a file, or the
  environment.
- Entry names, user names, URLs, notes, and field values are `fixture-...` strings. The `Password`
  and token fields are marked protected in the model, but the bytes are still fixture data. They are
  not a real secret.
- The vault is written into a fresh temporary directory, read by the benchmark, and deleted when the
  process exits. Nothing outside that directory is opened.
- The low-round vaults use 8 AES-KDF rounds so the provider's mapping and XML work is visible. This
  is a fixture setting; the `realistic-kdf-600000` vault uses the codec's default 600,000 rounds.
- The benchmark never prints a mapped value. The provider's snapshot holds mapped fields in
  plaintext by contract; the benchmark keeps the same rule and only `black_box`es the result.

No credential is read, and no real vault is used. The assessment is that a realistic KeePass
benchmark is possible without secret use; the only unresolvable part is testing the exact KDF a
given real vault chose, which is a deployment property rather than a fixture property.

## Raw output

The full Criterion logs for all four runs are in
`eng/benchmarks/2026-11-21-x86_64-linux/`:

- `config-run1.txt`, `config-run2.txt`, `config-run3.txt`, `config-run4.txt`
- `keepass-run1.txt`, `keepass-run2.txt`, `keepass-run3.txt`, `keepass-run4.txt`
- `environment.txt`

They are outside the crate directories, so they are part of this repository but never part of a
published `.crate`.
