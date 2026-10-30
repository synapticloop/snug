//! The per-user cache namespace, shared between the builder and the
//! launcher runtime.
//!
//! The launcher keeps a copy of the application's extracted JAR under a
//! per-user cache directory:
//!
//! ```text
//! <platform cache base>\snug\<company>\<app>\
//! ```
//!
//! Two things about that layout are *contract*, not implementation, and
//! both sides have to agree on them: the `snug` namespace segment, and
//! the rules for turning a company or application name into something
//! safe to use as a directory name. The builder shows the layout in the
//! build log, so the two cannot drift apart — which is why the rules
//! live here, in the crate both of them already depend on.
//!
//! What is deliberately *not* here is the platform's cache base. That is
//! per-user and per-machine (`%LOCALAPPDATA%` on Windows, `~/Library/
//! Caches` on macOS), and the launcher resolves it at *runtime*, on the
//! machine the app runs on — which is usually not the machine that built
//! it. The builder can therefore only name the convention, never the
//! path.

/// Default top-level subdirectory under the per-user data root.
pub const SNUG_SUBDIR: &str = "snug";

/// Replace path separators and other characters that are unsafe in a
/// directory name across the platforms we support.
pub fn sanitize_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// The path segments of an application's cache root, relative to the
/// platform's per-user cache base.
///
/// Segments rather than a `Path`, and that is not an accident: the
/// builder can be on one platform and produce an app for another, and
/// `Path::join` would use the *host's* separator. A Windows path
/// written with `/` is wrong to show a Windows user, so the caller
/// renders the segments with the separator of the platform it is
/// building for.
pub fn cache_root_segments(company: &str, name: &str) -> Vec<String> {
    vec![
        SNUG_SUBDIR.to_string(),
        sanitize_component(company),
        sanitize_component(name),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_safe_name_is_left_alone() {
        assert_eq!(
            sanitize_component("Example Company Pty Ltd"),
            "Example Company Pty Ltd"
        );
    }

    #[test]
    fn unsafe_characters_become_underscores() {
        assert_eq!(sanitize_component("a/b\\c:d*e"), "a_b_c_d_e");
        assert_eq!(sanitize_component("My\u{0001}App"), "My_App");
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(sanitize_component("   "), "");
        assert_eq!(sanitize_component("  Demo  "), "Demo");
    }

    #[test]
    fn the_root_is_namespaced_by_company_then_app() {
        let segments = cache_root_segments("Example Company Pty Ltd", "Demo");
        assert_eq!(segments, ["snug", "Example Company Pty Ltd", "Demo"]);
    }

    #[test]
    fn the_segments_are_sanitised() {
        let segments = cache_root_segments("Acme/Inc", "My:App");
        assert_eq!(segments, ["snug", "Acme_Inc", "My_App"]);
    }
}
