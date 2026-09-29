//! `--find-main`: list every class in the input JAR(s) that declares a
//! `public static void main(String[])`, then say whether the main
//! class snug would actually use is among them.
//!
//! This is a **read-only diagnostic**. It writes nothing, builds no
//! EXE, and never fails a build — including when the configured main
//! class isn't in the list. Knowing your entry points is a question;
//! rejecting your build over the answer is a different product
//! decision, and one that has to account for JavaFX `Application`
//! subclasses, which legitimately have no `main` at all (see
//! `snug-launcher`'s `Application.launch` fallback).
//!
//! Output goes to stdout so it pipes cleanly:
//!
//! ```text
//! snug App.jar --find-main > entry-points.txt
//! ```

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Result;

use crate::build::{jar_paths_in, resolve_input_path};
use crate::classfile::find_main_classes;
use crate::cli::Cli;
use crate::manifest::{MainClassDeclaration, read_main_class, survey_main_classes};

/// Where the main class snug would use came from, for labelling.
struct MainClassTarget {
    name: String,
    source: &'static str,
}

/// Entry point for `--find-main`. Resolves the input, scans each JAR,
/// and prints the report. Returns `Ok(())` in every non-fatal case.
pub fn run(cli: &Cli) -> Result<()> {
    let (input_path, is_dir) = resolve_input_path(cli)?;

    let jar_paths: Vec<PathBuf> = if is_dir {
        jar_paths_in(&input_path)?
    } else {
        vec![input_path.clone()]
    };

    let target = resolve_main_class_target(cli, &jar_paths);
    let declarations = survey_jars(&jar_paths);

    let multi_jar = jar_paths.len() > 1;
    println!(
        "snug: scanning {} for public static void main(String[]) entry points\n",
        if multi_jar {
            format!("{} JAR(s) in {}", jar_paths.len(), input_path.display())
        } else {
            input_path.display().to_string()
        }
    );

    // First JAR to declare a class wins, matching the order the
    // launcher puts them on `-classpath`. A duplicate across JARs is
    // the same class, so listing it twice would only add noise.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut total = 0usize;
    let mut oversized_total = 0usize;

    for jar_path in &jar_paths {
        let scan = find_main_classes(jar_path)?;
        oversized_total += scan.oversized;

        let fresh: Vec<&String> = scan
            .classes
            .iter()
            .filter(|class| seen.insert((*class).clone()))
            .collect();
        let fresh_count = fresh.len();

        if multi_jar {
            println!("  {}:", jar_path.display());
        }

        if fresh.is_empty() {
            if multi_jar {
                println!("    (none)");
            }
            continue;
        }

        for class in fresh {
            let marker = match &target {
                // Java class names are case-sensitive, so an exact
                // comparison is the correct one: `com.Exampel.Main`
                // genuinely is a different class and genuinely is a
                // bug.
                Some(target) if class == &target.name => format!("  <-- {}", target.source),
                _ => String::new(),
            };
            let indent = if multi_jar { "    " } else { "  " };
            println!("{indent}{class}{marker}");
        }
        total += fresh_count;
    }

    if total == 0 {
        println!();
        println!("No public static void main(String[]) method found.");
    } else {
        println!();
        println!("{total} main class(es) found.");
    }

    if oversized_total > 0 {
        println!(
            "Note: {oversized_total} class file(s) exceeded snug's scan window and may \
             declare a main that was not reported."
        );
    }

    report_ambiguous_manifests(&declarations, &target);

    println!();
    match target {
        Some(target) if total > 0 && !seen.contains(&target.name) => {
            println!(
                "Main class {} (from {}) is NOT among the classes found.",
                target.name, target.source
            );
            println!(
                "  That is not necessarily wrong — a JavaFX Application subclass has no \
                 main method, and snug's launcher supports those. Otherwise check the \
                 spelling."
            );
        }
        Some(target) if total == 0 => {
            println!(
                "Main class {} (from {}) could not be verified: no main classes were \
                 found in the JAR(s) at all.",
                target.name, target.source
            );
            // This is precisely the JavaFX shape — an `Application`
            // subclass, and nothing else in the JAR with a main. Say
            // so, because "nothing found" is otherwise alarming and the
            // user has no way to know their build is fine.
            println!(
                "  If it is a JavaFX Application subclass that is expected: it has no \
                 main method, and snug's launcher calls Application.launch() for it."
            );
        }
        Some(target) => {
            println!(
                "Main class {} (from {}) exists and declares a main method.",
                target.name, target.source
            );
        }
        None => {
            println!(
                "No main class resolved — pass --main-class <CLASS>, or add a \
                 Main-Class header to the JAR manifest."
            );
        }
    }

    Ok(())
}

/// Read every JAR's manifest and keep the ones that declare a
/// `Main-Class`.
///
/// Cheap by design: the central directory plus one small inflated
/// entry per JAR, against builds that are about to hash and embed
/// every byte of those same files.
fn survey_jars(jar_paths: &[PathBuf]) -> Vec<MainClassDeclaration> {
    survey_main_classes(jar_paths.iter().map(|path| {
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let bytes = std::fs::read(path).unwrap_or_default();
        (label, bytes)
    }))
}

/// Flag the "which manifest wins?" question when it has no answer.
///
/// The builder reads `Main-Class` from the **first** JAR, and "first"
/// means first by filename. That's a tie-break, not a policy: a
/// directory of JARs routinely holds more than one `Main-Class` — a fat
/// JAR alongside a bundled runnable tool, or a dependency that is
/// itself executable — and which one snug picks then depends on
/// filenames rather than anything anyone decided.
///
/// This is reported rather than resolved, because only the user knows
/// which entry point they meant, and because the alphabetical winner
/// is sometimes correct on purpose.
///
/// Suppressed when `--main-class` was passed: that is the user having
/// answered the question already.
fn report_ambiguous_manifests(
    declarations: &[MainClassDeclaration],
    target: &Option<MainClassTarget>,
) {
    if declarations.len() < 2 {
        return;
    }

    // Only flag when the target *came from* a manifest. A `--main-class`
    // override is an explicit choice and needs no defence.
    let Some(target) = target else { return };
    if target.source != "manifest Main-Class" {
        return;
    }

    // Several JARs declaring the *same* class is not a conflict — it's
    // a consistent input, and the answer doesn't depend on ordering.
    // Only disagreeing manifests make "which one wins?" a real
    // question.
    let distinct: BTreeSet<&str> = declarations.iter().map(|d| d.main_class.as_str()).collect();
    if distinct.len() < 2 {
        println!(
            "All {} JARs declaring a Main-Class agree on {}. No ambiguity.",
            declarations.len(),
            target.name
        );
        return;
    }

    println!(
        "Warning: {} JARs declare different Main-Class values:",
        distinct.len()
    );
    // `declarations` is in JAR order and `build_payload` reads the
    // first JAR, so the in-use row is the first *declaring* JAR. Mark
    // by position, never by comparing class names: two JARs can
    // declare the same class, and matching on the value would light up
    // every row.
    for (index, declaration) in declarations.iter().enumerate() {
        let marker = if index == 0 { "  <-- in use" } else { "" };
        println!(
            "  {:<32} {}{}",
            declaration.jar, declaration.main_class, marker
        );
    }
    println!(
        "  snug reads the Main-Class of the FIRST JAR by filename, so {} was chosen \
         by accident of naming.",
        target.name
    );
    println!("  Pass --main-class <CLASS> to choose explicitly.");
}

/// The main class snug would embed, and where it came from.
///
/// `--main-class` wins; otherwise the manifest of the **first** JAR,
/// matching `build_payload`. The scan is only a cross-check, so a
/// manifest we can't read is treated as "no target" rather than an
/// error — `find_main_classes` will surface an unreadable JAR
/// immediately afterwards.
fn resolve_main_class_target(cli: &Cli, jar_paths: &[PathBuf]) -> Option<MainClassTarget> {
    if let Some(name) = &cli.main_class {
        return Some(MainClassTarget {
            name: name.trim().to_string(),
            source: "--main-class",
        });
    }

    let first = jar_paths.first()?;
    let from_manifest = read_main_class(first).ok().flatten()?;
    Some(MainClassTarget {
        name: from_manifest.trim().to_string(),
        source: "manifest Main-Class",
    })
}
