//! Reproducible KeePass source benchmarks.
//!
//! The suite measures the published provider at three levels: the KDBX file read,
//! the codec's own load (KDF, decryption, XML, model), and
//! [`Source::load`](neotales_config::Source::load) after the provider has mapped
//! entry fields into configuration paths. A final build adds the core merge.
//!
//! The fixture vaults are generated from synthetic literals; see
//! `keepass/benches/support/mod.rs`. Two AES-KDF costs are measured: a low one
//! that keeps the rest of the load visible, and the codec's default 600,000
//! rounds. Real vaults may use Argon2id instead; that is not measured here.
//!
//! Run the whole suite with `--all-features` so the reload groups are present.

#![allow(
    missing_docs,
    unreachable_pub,
    clippy::doc_markdown,
    reason = "KeePass is the product name and benchmark targets publish no API docs"
)]

mod support;

use std::{hint::black_box, path::Path, time::Duration};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use neotales_config::{Builder, Source};
use neotales_keepass::{KdbxCredentials, KdbxFile, KdbxReadLimits};

use support::{Fixtures, MASTER_PASSWORD, REALISTIC_ENTRIES, SMALL_ENTRIES};

/// The suite defaults, so a plain `cargo bench` is bounded without extra flags.
fn configure() -> Criterion {
    Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_millis(800))
}

fn bench_vault(
    criterion: &mut Criterion,
    label: &str,
    path: &Path,
    entries: usize,
    realistic_kdf: bool,
) {
    let bytes = std::fs::metadata(path).expect("fixture metadata").len();
    let mut group = criterion.benchmark_group(format!("keepass/{label}"));
    group.throughput(Throughput::Bytes(bytes));
    if realistic_kdf {
        // Key derivation dominates; ten samples is enough to show its spread
        // without spending a minute on a single benchmark group.
        group.sample_size(10);
        group.warm_up_time(Duration::from_millis(200));
        group.measurement_time(Duration::from_secs(1));
    }

    group.bench_function("read", |bencher| {
        bencher.iter(|| {
            let bytes = std::fs::read(black_box(path)).expect("fixture read");
            black_box(bytes)
        });
    });

    // The codec alone: open, KDF, decrypt, decompress, parse XML, build the
    // document model. No mapping and no core merge.
    group.bench_function("codec-load", |bencher| {
        bencher.iter(|| {
            let file = std::fs::File::open(black_box(path)).expect("fixture open");
            let mut credentials =
                KdbxCredentials::password(MASTER_PASSWORD).expect("fixture credentials");
            black_box(
                KdbxFile::load(file, &mut credentials, &KdbxReadLimits::default())
                    .expect("codec load"),
            );
        });
    });

    // Validate once, outside the timed region, so a fixture that stopped
    // mapping fails before the measurement starts.
    let provider = support::provider(path, entries);
    let probe = provider.load().expect("fixture loads");
    assert!(probe.as_map().is_some(), "the vault root is a map");
    let configuration = Builder::new()
        .source("vault", support::provider(path, entries))
        .build()
        .expect("fixture builds");
    assert_eq!(
        configuration
            .get::<String>("entries.entry_00.user")
            .expect("mapped user"),
        "fixture-value-00-config.user"
    );

    // The provider: codec load plus explicit field-to-path mapping.
    group.bench_function("load", |bencher| {
        bencher.iter(|| {
            black_box(provider.load().expect("vault loads"));
        });
    });

    // The whole build. Provider construction is setup, not part of the
    // measurement: a real caller registers the provider once and builds.
    group.bench_function("build", |bencher| {
        bencher.iter_batched(
            || support::provider(path, entries),
            |provider| {
                let config = Builder::new()
                    .source("vault", provider)
                    .memory("overlay", support::overlay())
                    .build()
                    .expect("vault build");
                black_box(
                    config
                        .get::<String>("entries.entry_00.user")
                        .expect("overlay wins"),
                );
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();

    // A credential rotation and a replacement file both go through `refresh`;
    // an unchanged poll fingerprints the KDBX path without loading.
    #[cfg(feature = "reload")]
    {
        let reload = Builder::new()
            .source("vault", support::provider(path, entries))
            .memory("overlay", support::overlay())
            .build_reloadable()
            .expect("reloadable vault");
        let mut group = criterion.benchmark_group(format!("keepass/{label}"));
        group.bench_function("refresh", |bencher| {
            bencher.iter(|| black_box(reload.refresh()));
        });
        group.bench_function("refresh-unchanged", |bencher| {
            bencher.iter(|| black_box(reload.refresh_if_changed()));
        });
        group.finish();
    }
}

fn all_benches(criterion: &mut Criterion) {
    let fixtures = Fixtures::new();
    bench_vault(criterion, "small", fixtures.small(), SMALL_ENTRIES, false);
    bench_vault(
        criterion,
        "realistic",
        fixtures.realistic(),
        REALISTIC_ENTRIES,
        false,
    );
    bench_vault(
        criterion,
        "realistic-kdf-600000",
        fixtures.realistic_kdf(),
        REALISTIC_ENTRIES,
        true,
    );
}

criterion_group! {
    name = benches;
    config = configure();
    targets = all_benches
}
criterion_main!(benches);
