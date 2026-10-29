//! The effective build options, and where each one came from.
//!
//! This is the answer to "why did snug do *that*?" — the question a user
//! has when an options file, a command-line flag and a built-in default
//! disagree, and the one that is hardest to answer after the fact
//! because the merged argv looks the same whoever supplied a value.
//!
//! The table shows the value that was actually used and the origin of
//! that value. Origins are read from the provenance side-channel rather
//! than reconstructed, so an explicitly typed `--min-java 25` shows as
//! `[command line]` while an untouched `--min-java` shows as `[default]`.

use std::path::Path;

use clap::ArgMatches;

use crate::build::output_path;
use crate::cli::Cli;
use crate::provenance::{Merged, Origin};
use crate::report::redact_jvm_arg;

/// One row of the effective-options table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The option's name, as the user knows it.
    pub label: String,
    /// The value that was used.
    pub value: String,
    /// Where that value came from.
    pub origin: Origin,
    /// Whether the value was masked for display.
    pub masked: bool,
    /// The origin that beat this value, when this value lost.
    ///
    /// A row that carries one is an *overridden* value: it never reached
    /// the build, and this says what reached it instead.
    pub superseded_by: Option<Origin>,
}

impl Row {
    fn new(label: impl Into<String>, value: impl Into<String>, origin: Origin) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            origin,
            masked: false,
            superseded_by: None,
        }
    }

    fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    /// Mark this row as a value that lost to another origin.
    fn superseded_by(mut self, winner: Origin) -> Self {
        self.superseded_by = Some(winner);
        self
    }
}

/// Build the effective-options rows.
///
/// `main_class_source` is the origin of the main class when it did not
/// come from `--main-class` — that is, the JAR manifest it was read
/// from.
pub fn rows(
    cli: &Cli,
    merged: &Merged,
    matches: &ArgMatches,
    main_class_source: Option<Origin>,
) -> Vec<Row> {
    let mut rows = Vec::new();

    // --- input -----------------------------------------------------------
    let (input_value, input_origin) = match (&cli.jar, &cli.input) {
        (Some(path), _) => (path.display().to_string(), merged.origin_of(matches, "jar")),
        (None, Some(path)) => (
            path.display().to_string(),
            merged.origin_of(matches, "input"),
        ),
        (None, None) => ("(none)".to_string(), Origin::NotSet),
    };
    rows.push(Row::new("input", input_value, input_origin));

    // --- output ----------------------------------------------------------
    let output = output_path(cli);
    let output_origin = match &cli.output {
        Some(_) => merged.origin_of(matches, "output"),
        // The output is derived from the input when it was not named.
        None => Origin::Inferred {
            from: "input".to_string(),
        },
    };
    rows.push(Row::new(
        "output",
        output.display().to_string(),
        output_origin,
    ));

    // --- application details ---------------------------------------------
    rows.push(Row::new(
        "name",
        cli.name
            .clone()
            .unwrap_or_else(|| "(from input)".to_string()),
        match &cli.name {
            Some(_) => merged.origin_of(matches, "name"),
            None => Origin::Inferred {
                from: "input".to_string(),
            },
        },
    ));
    rows.push(Row::new(
        "company",
        cli.company
            .clone()
            .unwrap_or_else(|| "(from name)".to_string()),
        match &cli.company {
            Some(_) => merged.origin_of(matches, "company"),
            None => Origin::DefaultRule("application name".to_string()),
        },
    ));
    rows.push(Row::new(
        "version",
        cli.version.clone().unwrap_or_else(|| "0.0.0".to_string()),
        match &cli.version {
            Some(_) => merged.origin_of(matches, "version"),
            None => Origin::Default,
        },
    ));
    rows.push(Row::new(
        "description",
        cli.description
            .clone()
            .unwrap_or_else(|| "(not set)".to_string()),
        match &cli.description {
            Some(_) => merged.origin_of(matches, "description"),
            None => Origin::NotSet,
        },
    ));
    rows.push(Row::new(
        "copyright",
        cli.copyright
            .clone()
            .unwrap_or_else(|| "(not set)".to_string()),
        match &cli.copyright {
            Some(_) => merged.origin_of(matches, "copyright"),
            None => Origin::NotSet,
        },
    ));
    rows.push(Row::new(
        "update-url",
        cli.update_url
            .clone()
            .unwrap_or_else(|| "(not set)".to_string()),
        match &cli.update_url {
            Some(_) => merged.origin_of(matches, "update_url"),
            None => Origin::NotSet,
        },
    ));

    // --- entry point ------------------------------------------------------
    rows.push(Row::new(
        "main-class",
        cli.main_class
            .clone()
            .unwrap_or_else(|| "(from manifest)".to_string()),
        match (&cli.main_class, main_class_source) {
            (Some(_), _) => merged.origin_of(matches, "main_class"),
            (None, Some(origin)) => origin,
            (None, None) => Origin::NotSet,
        },
    ));
    rows.push(Row::new(
        "min-java",
        cli.min_java.to_string(),
        merged.origin_of(matches, "min_java"),
    ));

    // --- JVM arguments ----------------------------------------------------
    let jvm_origins = merged.origins_of_all(matches, "jvm_args");
    if jvm_origins.is_empty() {
        rows.push(Row::new("jvm-arg", "(none)".to_string(), Origin::NotSet));
    } else {
        for (value, origin) in cli.jvm_args.iter().zip(jvm_origins) {
            let redacted = redact_jvm_arg(value);
            let masked = redacted != *value;
            rows.push(Row::new("jvm-arg", redacted, origin).masked_if(masked));
        }
    }

    // --- resources --------------------------------------------------------
    rows.push(Row::new(
        "icon",
        cli.icon
            .as_ref()
            .map_or_else(|| "(not set)".to_string(), |p| p.display().to_string()),
        match &cli.icon {
            Some(_) => merged.origin_of(matches, "icon"),
            None => Origin::NotSet,
        },
    ));
    rows.push(Row::new(
        "splash",
        cli.splash
            .as_ref()
            .map_or_else(|| "(not set)".to_string(), |p| p.display().to_string()),
        match &cli.splash {
            Some(_) => merged.origin_of(matches, "splash"),
            None => Origin::NotSet,
        },
    ));
    rows.push(Row::new(
        "splash-ms",
        cli.splash_ms.to_string(),
        merged.origin_of(matches, "splash_ms"),
    ));
    rows.push(Row::new(
        "splash-max",
        cli.splash_max.clone(),
        merged.origin_of(matches, "splash_max"),
    ));
    rows.push(Row::new(
        "manifest",
        cli.manifest
            .as_ref()
            .map_or_else(|| "(not set)".to_string(), |p| p.display().to_string()),
        match &cli.manifest {
            Some(_) => merged.origin_of(matches, "manifest"),
            None => Origin::NotSet,
        },
    ));

    // --- build behaviour --------------------------------------------------
    rows.push(Row::new(
        "download-jdk",
        format!("{:?}", cli.download_jdk).to_lowercase(),
        merged.origin_of(matches, "download_jdk"),
    ));
    rows.push(Row::new(
        "cache-dir",
        cli.cache_dir
            .as_ref()
            .map_or_else(|| "(not set)".to_string(), |p| p.display().to_string()),
        match &cli.cache_dir {
            Some(_) => merged.origin_of(matches, "cache_dir"),
            None => Origin::NotSet,
        },
    ));
    rows.push(Row::new(
        "allow-multiple-instances",
        cli.allow_multiple_instances.to_string(),
        merged.origin_of(matches, "allow_multiple_instances"),
    ));

    // --- localisation ------------------------------------------------------
    let localisation_origins = merged.origins_of_all(matches, "localizations");
    if localisation_origins.is_empty() {
        rows.push(Row::new(
            "localisation",
            "(none)".to_string(),
            Origin::NotSet,
        ));
    } else {
        for (path, origin) in cli.localizations.iter().zip(localisation_origins) {
            rows.push(Row::new("localisation", path.display().to_string(), origin));
        }
    }

    // --- build log ---------------------------------------------------------
    rows.push(Row::new(
        "build-log",
        cli.build_log
            .as_ref()
            .map_or_else(|| "(default)".to_string(), |p| p.display().to_string()),
        match &cli.build_log {
            Some(_) => merged.origin_of(matches, "build_log"),
            None => Origin::Default,
        },
    ));

    // --- overridden values -------------------------------------------------
    //
    // The values that lost. They never reached the build, so they are not
    // part of the effective configuration — but a user asking "why did snug
    // ignore my `snug.options`?" needs to see what was ignored and what beat
    // it, and the merge has already done the work of working that out.
    //
    // The winner's origin is read back out of the merged argv rather than
    // carried along in the record, because the record is made while the loser's
    // layer is being processed and the winner may not have been seen yet.
    let mut losers: Vec<(String, Vec<Row>)> = Vec::new();
    for lost in &merged.overridden {
        // The winner's origin is the one already computed for the winner's
        // own row, so the two cannot disagree about where the value came
        // from. Asking the merged argv instead would answer "not set" for
        // `--input` when the command line's *positional* is what beat it:
        // the two are the same setting, but only one of them is a flag.
        let winner = rows
            .iter()
            .find(|row| row.label == lost.flag)
            .map(|row| row.origin.clone())
            .unwrap_or_else(|| {
                merged
                    .id_for(&lost.flag)
                    .map(|id| merged.origin_of(matches, id))
                    .unwrap_or(Origin::CommandLine)
            });
        let row = Row::new(
            lost.flag.clone(),
            lost.value.clone().unwrap_or_else(|| "(unset)".to_string()),
            lost.origin.clone(),
        )
        .superseded_by(winner);
        match losers
            .iter_mut()
            .find(|(flag, _)| flag.as_str() == lost.flag.as_str())
        {
            Some((_, rows)) => rows.push(row),
            None => losers.push((lost.flag.clone(), vec![row])),
        }
    }

    // Each loser sits directly under the value that beat it, so the two read as
    // a pair rather than as two unrelated lines.
    let mut table: Vec<Row> = Vec::with_capacity(rows.len() + losers.len());
    let mut placed = vec![false; losers.len()];
    for row in rows {
        let label = row.label.clone();
        table.push(row);
        for (index, (flag, extra)) in losers.iter().enumerate() {
            if flag == &label && !placed[index] {
                placed[index] = true;
                table.extend(extra.iter().cloned());
            }
        }
    }
    // A loser whose flag has no row of its own still belongs in the log: it was
    // a setting the user wrote, and it was ignored.
    for (index, (_, extra)) in losers.iter().enumerate() {
        if !placed[index] {
            table.extend(extra.iter().cloned());
        }
    }

    table
}

impl Row {
    /// Mark the row masked, only when it actually is.
    fn masked_if(self, masked: bool) -> Self {
        if masked { self.masked() } else { self }
    }
}

/// Render the effective-options table.
///
/// The origin column is aligned, because a table whose third column
/// wanders is much harder to scan than one whose values line up.
pub fn render(rows: &[Row], cwd: &Path) -> String {
    let mut out = String::from("Effective build options:\n");
    if rows.is_empty() {
        return out;
    }

    // The colon is part of the label, so the width has to include it —
    // otherwise "input:" and "output:" end in different columns and the
    // values below them do not line up.
    let label_width = rows.iter().map(|r| r.label.len() + 1).max().unwrap_or(0);
    let value_width = rows.iter().map(|r| r.value.len()).max().unwrap_or(0);

    for row in rows {
        let origin = row.origin.label(cwd);
        let mut line = format!(
            "  {:<label_width$}  {:<value_width$}  {}",
            format!("{}:", row.label),
            row.value,
            origin
        );
        if let Some(winner) = &row.superseded_by {
            line.push_str(&format!("  — overridden by {}", winner.label(cwd)));
        }
        if row.masked {
            line.push_str("  (masked)");
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::Origin;

    #[test]
    fn the_table_aligns_its_columns() {
        let rows = vec![
            Row::new("input", "app.jar", Origin::CommandLine),
            Row::new(
                "output",
                "app.exe",
                Origin::Inferred {
                    from: "input".into(),
                },
            ),
        ];
        let rendered = render(&rows, Path::new("/work"));
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "Effective build options:");
        assert_eq!(lines[1], "  input:   app.jar  [command line]");
        assert_eq!(lines[2], "  output:  app.exe  [inferred from input]");
    }

    #[test]
    fn a_masked_row_says_so() {
        let rows = vec![Row::new("jvm-arg", "-Dapi.token=***", Origin::CommandLine).masked()];
        let rendered = render(&rows, Path::new("/work"));
        assert!(rendered.contains("(masked)"), "{rendered}");
    }
}
