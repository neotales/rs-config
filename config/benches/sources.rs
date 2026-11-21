//! Reproducible source-load benchmarks for `neotales-config`.
//!
//! The suite measures a file provider from the filesystem through its published
//! [`Source::load`](neotales_config::Source::load), and then through
//! [`Builder::build`](neotales_config::Builder::build) with a small in-memory
//! overlay. For each format it also measures the file read alone, so the
//! I/O portion of a load is visible, and, where a parser is reachable through
//! its own public API, the parser alone. The in-memory groups are the merge
//! baseline.
//!
//! Feature gates are the library's own: a provider group exists only when its
//! feature is enabled, and the in-memory groups always run. Run the whole suite
//! with `--all-features`. The fixture generator is deterministic and produces
//! synthetic non-secret data; see `config/docs/benchmarks.md`.

#![allow(missing_docs, unreachable_pub)]

mod support;

use std::{hint::black_box, time::Duration};

#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini",
    feature = "dotenv"
))]
use std::path::Path;

#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini",
    feature = "dotenv"
))]
use criterion::Throughput;
use criterion::{Criterion, criterion_group, criterion_main};
use neotales_config::Builder;
#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini",
    feature = "dotenv"
))]
use neotales_config::Source;

use support::Size;
#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini",
    feature = "dotenv"
))]
use support::{Fixtures, Format};

/// The suite defaults, so a plain `cargo bench` is bounded without extra flags.
///
/// Command-line values still override these, which is how a longer run is
/// requested. Twenty samples is enough to show spread on one machine; it is
/// not a statistical claim about another.
fn configure() -> Criterion {
    Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_millis(800))
}

/// Runs one size of one provider: read, optional parser-only, load, build.
#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini",
    feature = "dotenv"
))]
#[allow(
    clippy::too_many_arguments,
    reason = "a benchmark row is described by its phases; grouping them would add a struct \
              whose only purpose is this call"
)]
fn run_case(
    criterion: &mut Criterion,
    provider: &str,
    size: Size,
    path: &Path,
    bytes: u64,
    parse: Option<&dyn Fn()>,
    load: &dyn Fn(),
    build: &dyn Fn(),
) {
    let mut group = criterion.benchmark_group(format!("{provider}/{}", size.label()));
    group.throughput(Throughput::Bytes(bytes));

    // Read only: the I/O half of every file provider, without a parser.
    group.bench_function("read", |bencher| {
        bencher.iter(|| {
            let bytes = std::fs::read(black_box(path)).expect("fixture read");
            black_box(String::from_utf8(bytes).expect("fixture is UTF-8"))
        });
    });

    if let Some(parse) = parse {
        group.bench_function("parse", |bencher| bencher.iter(parse));
    }

    // `Source::load`: read, decode, parse, and convert into the value model.
    group.bench_function("load", |bencher| bencher.iter(load));

    // The whole build: load, merge the overlay, and record provenance.
    group.bench_function("build", |bencher| bencher.iter(build));

    group.finish();
}

/// Runs the reload phases for one provider using the generic `Builder::source`.
///
/// The fixture is never modified, so every timed closure must return
/// [`neotales_config::RefreshOutcome::Unchanged`]. The assertion is inside the
/// closure: a failed refresh, or a reload that silently produced a different
/// snapshot, fails the benchmark instead of being recorded as a refresh time.
#[cfg(all(
    feature = "reload",
    any(
        feature = "json",
        feature = "json5",
        feature = "jsonc",
        feature = "toml",
        feature = "yaml",
        feature = "ini",
        feature = "dotenv"
    )
))]
fn run_reload<S>(criterion: &mut Criterion, provider: &str, size: Size, source: S)
where
    S: Source + Clone + 'static,
{
    let reload = Builder::new()
        .source("file", source)
        .memory("overlay", support::overlay())
        .build_reloadable()
        .expect("reloadable build");

    let mut group = criterion.benchmark_group(format!("{provider}/{}", size.label()));

    // Unchanged-snapshot refresh: fingerprint, reload, merge, and compare.
    // The fixture does not change, so the outcome is `Unchanged` and nothing
    // is published. Changed-snapshot publication is not measured here.
    group.bench_function("refresh", |bencher| {
        bencher.iter(|| {
            let outcome = reload.refresh();
            assert_eq!(outcome, neotales_config::RefreshOutcome::Unchanged);
            black_box(outcome);
        });
    });

    // The polling path over an unchanged file: fingerprint and compare only,
    // with no source load and no publication.
    group.bench_function("refresh-unchanged", |bencher| {
        bencher.iter(|| {
            let outcome = reload.refresh_if_changed();
            assert_eq!(outcome, neotales_config::RefreshOutcome::Unchanged);
            black_box(outcome);
        });
    });

    group.finish();
}

fn memory(criterion: &mut Criterion) {
    for size in Size::BOTH {
        let defaults = support::value(size);
        let mut group = criterion.benchmark_group(format!("memory/{}", size.label()));
        group.bench_function("build", |bencher| {
            bencher.iter(|| {
                let config = Builder::new()
                    .memory("defaults", defaults.clone())
                    .build()
                    .expect("memory build");
                black_box(config.get::<u16>("http.port").expect("http.port"));
            });
        });
        group.finish();
    }

    // Two sources, so the map-merge path is measured on its own.
    let defaults = support::value(Size::Realistic);
    let mut group = criterion.benchmark_group("memory/realistic-merge");
    group.bench_function("build", |bencher| {
        bencher.iter(|| {
            let config = Builder::new()
                .memory("defaults", defaults.clone())
                .memory("overlay", support::overlay())
                .build()
                .expect("memory merge build");
            black_box(config.get::<u16>("http.port").expect("http.port"));
        });
    });
    group.finish();
}

#[cfg(feature = "json")]
fn json_provider(criterion: &mut Criterion, fixtures: &Fixtures) {
    for size in Size::BOTH {
        let path = fixtures.path(Format::Json, size).to_path_buf();
        let text = std::fs::read_to_string(&path).expect("fixture");
        let provider = neotales_config::JsonFile::new(path.clone());

        let probe = provider.load().expect("json fixture loads");
        assert!(probe.as_map().is_some(), "json fixture root is a map");
        let configuration = Builder::new()
            .json_file("file", provider.clone())
            .build()
            .expect("json fixture builds");
        assert_eq!(
            configuration.get::<u16>("http.port").expect("http.port"),
            8080
        );

        let parse = move || {
            black_box(serde_json::from_str::<serde_json::Value>(&text).expect("json parses"));
        };
        let load_provider = provider.clone();
        let load = move || {
            black_box(load_provider.load().expect("json loads"));
        };
        let build_provider = provider.clone();
        let build = move || {
            let config = Builder::new()
                .json_file("file", build_provider.clone())
                .memory("overlay", support::overlay())
                .build()
                .expect("json build");
            black_box(config.get::<u16>("http.port").expect("overlay wins"));
        };

        run_case(
            criterion,
            "json",
            size,
            &path,
            fixtures.bytes(Format::Json, size),
            Some(&parse),
            &load,
            &build,
        );
        #[cfg(feature = "reload")]
        run_reload(criterion, "json", size, provider);
    }
}

#[cfg(feature = "json5")]
fn json5_provider(criterion: &mut Criterion, fixtures: &Fixtures) {
    for size in Size::BOTH {
        let path = fixtures.path(Format::Json5, size).to_path_buf();
        let text = std::fs::read_to_string(&path).expect("fixture");
        let provider = neotales_config::Json5File::new(path.clone());

        let probe = provider.load().expect("json5 fixture loads");
        assert!(probe.as_map().is_some(), "json5 fixture root is a map");
        let configuration = Builder::new()
            .json5_file("file", provider.clone())
            .build()
            .expect("json5 fixture builds");
        assert_eq!(
            configuration.get::<u16>("http.port").expect("http.port"),
            8080
        );

        // JSON5 has no value type of its own, so a generic value is the
        // parser-only target. The provider additionally enforces exact number
        // handling; this baseline does not.
        let parse = move || {
            black_box(json5::from_str::<serde_json::Value>(&text).expect("json5 parses"));
        };
        let load_provider = provider.clone();
        let load = move || {
            black_box(load_provider.load().expect("json5 loads"));
        };
        let build_provider = provider.clone();
        let build = move || {
            let config = Builder::new()
                .json5_file("file", build_provider.clone())
                .memory("overlay", support::overlay())
                .build()
                .expect("json5 build");
            black_box(config.get::<u16>("http.port").expect("overlay wins"));
        };

        run_case(
            criterion,
            "json5",
            size,
            &path,
            fixtures.bytes(Format::Json5, size),
            Some(&parse),
            &load,
            &build,
        );
        #[cfg(feature = "reload")]
        run_reload(criterion, "json5", size, provider);
    }
}

#[cfg(feature = "jsonc")]
fn jsonc_provider(criterion: &mut Criterion, fixtures: &Fixtures) {
    for size in Size::BOTH {
        let path = fixtures.path(Format::Jsonc, size).to_path_buf();
        let text = std::fs::read_to_string(&path).expect("fixture");
        let provider = neotales_config::JsoncFile::new(path.clone());

        let probe = provider.load().expect("jsonc fixture loads");
        assert!(probe.as_map().is_some(), "jsonc fixture root is a map");
        let configuration = Builder::new()
            .jsonc_file("file", provider.clone())
            .build()
            .expect("jsonc fixture builds");
        assert_eq!(
            configuration.get::<u16>("http.port").expect("http.port"),
            8080
        );

        // The same options the provider uses: comments only.
        let parse = move || {
            let options = jsonc_parser::ParseOptions {
                allow_comments: true,
                allow_loose_object_property_names: false,
                allow_trailing_commas: false,
                allow_missing_commas: false,
                allow_single_quoted_strings: false,
                allow_hexadecimal_numbers: false,
                allow_unary_plus_numbers: false,
                allow_bare_decimal_point_numbers: false,
                allow_non_finite_numbers: false,
                allow_extended_string_escapes: false,
            };
            black_box(
                jsonc_parser::parse_to_value(&text, &options)
                    .expect("jsonc parses")
                    .expect("jsonc has a document"),
            );
        };
        let load_provider = provider.clone();
        let load = move || {
            black_box(load_provider.load().expect("jsonc loads"));
        };
        let build_provider = provider.clone();
        let build = move || {
            let config = Builder::new()
                .jsonc_file("file", build_provider.clone())
                .memory("overlay", support::overlay())
                .build()
                .expect("jsonc build");
            black_box(config.get::<u16>("http.port").expect("overlay wins"));
        };

        run_case(
            criterion,
            "jsonc",
            size,
            &path,
            fixtures.bytes(Format::Jsonc, size),
            Some(&parse),
            &load,
            &build,
        );
        #[cfg(feature = "reload")]
        run_reload(criterion, "jsonc", size, provider);
    }
}

#[cfg(feature = "toml")]
fn toml_provider(criterion: &mut Criterion, fixtures: &Fixtures) {
    for size in Size::BOTH {
        let path = fixtures.path(Format::Toml, size).to_path_buf();
        let text = std::fs::read_to_string(&path).expect("fixture");
        let provider = neotales_config::TomlFile::new(path.clone());

        let probe = provider.load().expect("toml fixture loads");
        assert!(probe.as_map().is_some(), "toml fixture root is a map");
        let configuration = Builder::new()
            .toml_file("file", provider.clone())
            .build()
            .expect("toml fixture builds");
        assert_eq!(
            configuration.get::<u16>("http.port").expect("http.port"),
            8080
        );

        let parse = move || {
            black_box(toml::from_str::<toml::Value>(&text).expect("toml parses"));
        };
        let load_provider = provider.clone();
        let load = move || {
            black_box(load_provider.load().expect("toml loads"));
        };
        let build_provider = provider.clone();
        let build = move || {
            let config = Builder::new()
                .toml_file("file", build_provider.clone())
                .memory("overlay", support::overlay())
                .build()
                .expect("toml build");
            black_box(config.get::<u16>("http.port").expect("overlay wins"));
        };

        run_case(
            criterion,
            "toml",
            size,
            &path,
            fixtures.bytes(Format::Toml, size),
            Some(&parse),
            &load,
            &build,
        );
        #[cfg(feature = "reload")]
        run_reload(criterion, "toml", size, provider);
    }
}

#[cfg(feature = "yaml")]
fn yaml_provider(criterion: &mut Criterion, fixtures: &Fixtures) {
    use saphyr::LoadableYamlNode as _;

    for size in Size::BOTH {
        let path = fixtures.path(Format::Yaml, size).to_path_buf();
        let text = std::fs::read_to_string(&path).expect("fixture");
        let provider = neotales_config::YamlFile::new(path.clone());

        let probe = provider.load().expect("yaml fixture loads");
        assert!(probe.as_map().is_some(), "yaml fixture root is a map");
        let configuration = Builder::new()
            .yaml_file("file", provider.clone())
            .build()
            .expect("yaml fixture builds");
        assert_eq!(
            configuration.get::<u16>("http.port").expect("http.port"),
            8080
        );

        // The parser crate's own document model; the provider instead drives
        // `saphyr-parser` event by event and applies its own number rules.
        let parse = move || {
            black_box(saphyr::Yaml::load_from_str(&text).expect("yaml parses"));
        };
        let load_provider = provider.clone();
        let load = move || {
            black_box(load_provider.load().expect("yaml loads"));
        };
        let build_provider = provider.clone();
        let build = move || {
            let config = Builder::new()
                .yaml_file("file", build_provider.clone())
                .memory("overlay", support::overlay())
                .build()
                .expect("yaml build");
            black_box(config.get::<u16>("http.port").expect("overlay wins"));
        };

        run_case(
            criterion,
            "yaml",
            size,
            &path,
            fixtures.bytes(Format::Yaml, size),
            Some(&parse),
            &load,
            &build,
        );
        #[cfg(feature = "reload")]
        run_reload(criterion, "yaml", size, provider);
    }
}

#[cfg(feature = "ini")]
fn ini_provider(criterion: &mut Criterion, fixtures: &Fixtures) {
    for size in Size::BOTH {
        let path = fixtures.path(Format::Ini, size).to_path_buf();
        let provider = neotales_config::IniFile::new(path.clone());

        let probe = provider.load().expect("ini fixture loads");
        assert!(probe.as_map().is_some(), "ini fixture root is a map");
        let configuration = Builder::new()
            .ini_file("file", provider.clone())
            .build()
            .expect("ini fixture builds");
        assert_eq!(
            configuration.get::<String>("http.port").expect("http.port"),
            "8080"
        );

        // The INI parser is private to the crate, so there is no public
        // parser-only baseline. `load` is read + parse + convert.
        let load_provider = provider.clone();
        let load = move || {
            black_box(load_provider.load().expect("ini loads"));
        };
        let build_provider = provider.clone();
        let build = move || {
            let config = Builder::new()
                .ini_file("file", build_provider.clone())
                .memory("overlay", support::overlay())
                .build()
                .expect("ini build");
            black_box(config.get::<u16>("http.port").expect("overlay wins"));
        };

        run_case(
            criterion,
            "ini",
            size,
            &path,
            fixtures.bytes(Format::Ini, size),
            None,
            &load,
            &build,
        );
        #[cfg(feature = "reload")]
        run_reload(criterion, "ini", size, provider);
    }
}

#[cfg(feature = "dotenv")]
fn dotenv_provider(criterion: &mut Criterion, fixtures: &Fixtures) {
    for size in Size::BOTH {
        let path = fixtures.path(Format::Dotenv, size).to_path_buf();
        let text = std::fs::read_to_string(&path).expect("fixture");
        let provider = neotales_config::DotenvFile::new(path.clone()).prefix("APP_");

        let probe = provider.load().expect("dotenv fixture loads");
        assert!(probe.as_map().is_some(), "dotenv fixture root is a map");
        let configuration = Builder::new()
            .dotenv_file("file", provider.clone())
            .build()
            .expect("dotenv fixture builds");
        assert_eq!(
            configuration.get::<String>("http.port").expect("http.port"),
            "8080"
        );

        // Parser only: the assignment list, without expansion or key mapping.
        let parse = move || {
            black_box(neotales_dotenv::parse(&text));
        };
        let load_provider = provider.clone();
        let load = move || {
            black_box(load_provider.load().expect("dotenv loads"));
        };
        let build_provider = provider.clone();
        let build = move || {
            let config = Builder::new()
                .dotenv_file("file", build_provider.clone())
                .memory("overlay", support::overlay())
                .build()
                .expect("dotenv build");
            black_box(config.get::<u16>("http.port").expect("overlay wins"));
        };

        run_case(
            criterion,
            "dotenv",
            size,
            &path,
            fixtures.bytes(Format::Dotenv, size),
            Some(&parse),
            &load,
            &build,
        );
        #[cfg(feature = "reload")]
        run_reload(criterion, "dotenv", size, provider);
    }
}

fn all_benches(criterion: &mut Criterion) {
    memory(criterion);

    #[cfg(any(
        feature = "json",
        feature = "json5",
        feature = "jsonc",
        feature = "toml",
        feature = "yaml",
        feature = "ini",
        feature = "dotenv"
    ))]
    {
        let fixtures = Fixtures::new();
        #[cfg(feature = "json")]
        json_provider(criterion, &fixtures);
        #[cfg(feature = "json5")]
        json5_provider(criterion, &fixtures);
        #[cfg(feature = "jsonc")]
        jsonc_provider(criterion, &fixtures);
        #[cfg(feature = "toml")]
        toml_provider(criterion, &fixtures);
        #[cfg(feature = "yaml")]
        yaml_provider(criterion, &fixtures);
        #[cfg(feature = "ini")]
        ini_provider(criterion, &fixtures);
        #[cfg(feature = "dotenv")]
        dotenv_provider(criterion, &fixtures);
    }
}

criterion_group! {
    name = benches;
    config = configure();
    targets = all_benches
}
criterion_main!(benches);
