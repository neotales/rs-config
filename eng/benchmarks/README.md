# Benchmark records

Raw Criterion output from local benchmark runs, kept in the repository so a figure in
`config/docs/benchmarks.md` can be traced back to a run. This directory is outside every crate
directory, so it is never part of a published `.crate`.

Each dated directory contains:

- `config-run*.txt` — `cargo bench -p neotales-config --all-features --bench sources`
- `keepass-run*.txt` — `cargo bench -p neotales-config-keepass --all-features --bench keepass`
- `environment.txt` — machine, toolchain, affinity, and the exact commands

The runs are observations on one host. Read `config/docs/benchmarks.md` for what each phase means,
the fixture shapes, and the listed limitations. Do not compare figures across machines or toolchains
without rerunning the suite there.
