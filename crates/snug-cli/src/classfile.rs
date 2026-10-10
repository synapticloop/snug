//! Find every class in a JAR that declares a `public static void
//! main(String[])` entry point.
//!
//! This is a **diagnostic**, not a validation gate. It answers "which
//! classes in this fat JAR are runnable?" so the user can pick a value
//! for `--main-class` (or confirm the one already configured) without
//! guessing. It never fails a build and never infers a value on the
//! user's behalf.
//!
//! ## How much of the class file we parse
//!
//! Only the *method table*. A class file is laid out (JVMS §4) as:
//!
//! ```text
//! magic | minor | major | constant_pool_count | constant_pool[] |
//! access_flags | this_class | super_class | interfaces[] |
//! fields_count | fields[] | methods_count | methods[] | attributes[]
//! ```
//!
//! Everything we need sits before the trailing class-level attributes,
//! and within a `method_info` we need just three `u2`s:
//!
//! ```text
//! access_flags | name_index | descriptor_index | attributes_count | attributes[]
//! ```
//!
//! Attribute blobs are skipped by their declared length, so we never
//! touch the `Code` attribute — no bytecode, no disassembly, no
//! control-flow analysis. A "has a main method" question is a
//! constant-pool lookup.
//!
//! ## Why a bounded prefix read
//!
//! The constant pool is variable-length, so there is no fixed offset
//! at which the method table begins and no way to seek straight to it.
//! We read a bounded prefix of each entry and parse forward. See
//! [`MAX_CLASS_PREFIX`] for the cap and what it costs.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

/// Maximum number of bytes read from the front of each `.class` entry.
///
/// The method table always sits near the *start* of a class file (the
/// constant pool is the only unbounded part ahead of it), so 64 KB
/// covers every class any real toolchain emits. Entries larger than
/// this are still scanned — we just can't see past the cap, and the
/// caller is told how many were affected so it can say so rather than
/// silently under-report.
pub const MAX_CLASS_PREFIX: usize = 64 * 1024;

/// JVM `ACC_PUBLIC` (JVMS §4.1).
const ACC_PUBLIC: u16 = 0x0001;

/// JVM `ACC_STATIC` (JVMS §4.1).
const ACC_STATIC: u16 = 0x0008;

/// Class-file magic, `0xCAFEBABE`.
const CLASS_MAGIC: u32 = 0xCAFE_BABE;

/// The only descriptor the JVM launcher accepts for an entry point.
const MAIN_DESCRIPTOR: &str = "([Ljava/lang/String;)V";

/// The name the JVM launcher looks up.
const MAIN_NAME: &str = "main";

/// Outcome of scanning one JAR for entry points.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MainScan {
    /// Fully-qualified class names (dots, not slashes) declaring
    /// `public static void main(String[])`, sorted and deduplicated.
    pub classes: Vec<String>,

    /// Number of `.class` entries whose uncompressed size exceeds
    /// [`MAX_CLASS_PREFIX`], so their method table may lie beyond the
    /// scanned window. Zero in the overwhelming majority of JARs.
    pub oversized: usize,
}

/// Scan a single JAR for `public static void main(String[])` classes.
///
/// Only `.class` entries are considered, and `module-info.class` is
/// skipped (it declares no methods by construction). Entries that fail
/// to parse — truncated, obfuscated, multi-release oddities — are
/// skipped silently; this is a best-effort diagnostic and a
/// non-conforming entry is not the user's problem to solve here.
pub fn find_main_classes(jar_path: &Path) -> Result<MainScan> {
    let file = std::fs::File::open(jar_path)
        .with_context(|| format!("opening JAR {}", jar_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("reading JAR {} as a zip archive", jar_path.display()))?;

    let mut classes = BTreeSet::new();
    let mut oversized = 0usize;

    for index in 0..archive.len() {
        // A broken entry shouldn't abort the whole scan; skip it.
        let mut entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        // `name()` borrows the entry, and we need `&mut entry` to read
        // it, so clone the (short) name out before touching the reader.
        let name = entry.name().to_owned();
        if !name.ends_with(".class") || name == "module-info.class" {
            continue;
        }
        // Skip multi-release overlays. A JAR with `Multi-Release: true`
        // carries versioned copies of ordinary classes under
        // `META-INF/versions/<n>/`, and this used to report every one of
        // them as a launchable entry point -- after `class_name_from_entry`
        // folded `/` to `.`, as `META-INF.versions.17.com.example.Main`.
        // That is not a name anyone can pass to `--main-class`, and it is
        // precisely the diagnostic `--find-main` exists to print for
        // someone to copy and paste. Only the root copy is launchable.
        if name.starts_with("META-INF/versions/") {
            continue;
        }

        let full_size = entry.size();
        let truncated = full_size > MAX_CLASS_PREFIX as u64;
        if truncated {
            oversized += 1;
        }

        // Size the buffer to what we'll actually read, not to the cap.
        // Reserving the full 64 KB up front would churn ~300 MB of
        // allocations across a 5,000-class JAR, most of it never
        // touched — on a debug build that alone cost more than the
        // parse.
        let mut buf = Vec::with_capacity(full_size.min(MAX_CLASS_PREFIX as u64) as usize);
        // Stop reading at the cap rather than inflating the whole
        // entry. Dropping the reader early skips the CRC check, which
        // is fine — we are not extracting anything, and we have no
        // reason to trust a JAR we are about to embed on that basis
        // any less than one we are about to read in full.
        let _ = entry
            .by_ref()
            .take(MAX_CLASS_PREFIX as u64)
            .read_to_end(&mut buf);

        if has_public_static_main(&buf) {
            classes.insert(class_name_from_entry(&name));
        }
    }

    Ok(MainScan {
        classes: classes.into_iter().collect(),
        oversized,
    })
}

/// Does this class file declare `public static void main(String[])`?
///
/// Returns `false` for anything unparseable. A truncated buffer (see
/// [`MAX_CLASS_PREFIX`]) parses as far as it can and then reports
/// `false` — never a panic, never a false positive.
pub fn has_public_static_main(bytes: &[u8]) -> bool {
    let Some(methods) = parse_method_table(bytes) else {
        return false;
    };
    methods.iter().any(|m| {
        m.access_flags & ACC_PUBLIC != 0
            && m.access_flags & ACC_STATIC != 0
            && m.name == MAIN_NAME
            && m.descriptor == MAIN_DESCRIPTOR
    })
}

/// A single `method_info`, reduced to the three fields we care about.
#[derive(Debug)]
struct Method {
    access_flags: u16,
    name: String,
    descriptor: String,
}

/// Parse forward to the method table and return it.
///
/// `None` means "not a class file I could read" — wrong magic,
/// truncated, or a constant-pool tag from a newer class-file version
/// we don't know. All are non-fatal for a diagnostic scan.
fn parse_method_table(bytes: &[u8]) -> Option<Vec<Method>> {
    let mut cur = Cursor::new(bytes);

    if cur.u4()? != CLASS_MAGIC {
        return None;
    }
    let _minor_version = cur.u2()?;
    let _major_version = cur.u2()?;

    let utf8 = parse_constant_pool(&mut cur)?;

    let _access_flags = cur.u2()?;
    let _this_class = cur.u2()?;
    let _super_class = cur.u2()?;

    let interface_count = cur.u2()? as usize;
    cur.skip(interface_count.checked_mul(2)?)?;

    // Fields come first and have the same shape as methods, so they
    // have to be walked to reach the methods. We parse and discard.
    let _fields = parse_members(&mut cur, &utf8)?;

    parse_members(&mut cur, &utf8)
}

/// Parse the constant pool, keeping only `CONSTANT_Utf8` payloads.
///
/// Every other constant type is skipped by its fixed size. The two
/// 8-byte types (`Long`, `Double`) occupy **two** constant-pool slots
/// per JVMS §4.4, which is the one place a naive parser silently
/// desynchronises and then reports nonsense — so it's called out here.
fn parse_constant_pool(cur: &mut Cursor<'_>) -> Option<Vec<String>> {
    let count = cur.u2()? as usize;
    // Index 0 is unused by the spec; allocate one spare so the vector
    // is directly indexable by constant-pool index.
    let mut utf8 = vec![String::new(); count.max(1)];

    let mut index = 1usize;
    while index < count {
        let tag = cur.u1()?;
        match tag {
            // CONSTANT_Utf8: u2 length + u1 bytes[length]
            1 => {
                let len = cur.u2()? as usize;
                let bytes = cur.take(len)?;
                utf8[index] = String::from_utf8_lossy(bytes).into_owned();
            }
            // Class(7) String(8) MethodType(16) Module(19) Package(20): u2
            7 | 8 | 16 | 19 | 20 => {
                cur.skip(2)?;
            }
            // MethodHandle(15): u1 reference_kind + u2 reference_index
            15 => {
                cur.skip(3)?;
            }
            // Integer(3) Float(4) Fieldref(9) Methodref(10)
            // InterfaceMethodref(11) NameAndType(12) Dynamic(17)
            // InvokeDynamic(18): u4
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => {
                cur.skip(4)?;
            }
            // Long(5) Double(6): u8, and the *next* slot is unusable.
            5 | 6 => {
                cur.skip(8)?;
                index += 1;
            }
            _ => return None,
        }
        index += 1;
    }

    Some(utf8)
}

/// Parse a `field_info[]` or `method_info[]` table.
fn parse_members(cur: &mut Cursor<'_>, utf8: &[String]) -> Option<Vec<Method>> {
    let count = cur.u2()? as usize;
    let mut members = Vec::with_capacity(count.min(1024));

    for _ in 0..count {
        let access_flags = cur.u2()?;
        let name_index = cur.u2()? as usize;
        let descriptor_index = cur.u2()? as usize;
        // Attributes are skipped wholesale by length — this is what
        // keeps us out of `Code` and off the bytecode entirely.
        skip_attributes(cur)?;

        members.push(Method {
            access_flags,
            name: utf8.get(name_index).cloned().unwrap_or_default(),
            descriptor: utf8.get(descriptor_index).cloned().unwrap_or_default(),
        });
    }

    Some(members)
}

/// Skip an `attribute_info[]` table using the declared lengths.
fn skip_attributes(cur: &mut Cursor<'_>) -> Option<()> {
    let count = cur.u2()? as usize;
    for _ in 0..count {
        let _attribute_name_index = cur.u2()?;
        let length = cur.u4()? as usize;
        cur.skip(length)?;
    }
    Some(())
}

/// Turn a zip entry name into a binary class name.
///
/// `com/example/Main.class` becomes `com.example.Main`. Nested classes
/// keep their `$` separator (`com/example/Outer$Inner.class`), which is
/// what you'd pass to `--main-class` to launch one directly.
fn class_name_from_entry(entry_name: &str) -> String {
    entry_name
        .strip_suffix(".class")
        .unwrap_or(entry_name)
        .replace('/', ".")
}

/// A bounds-checked, `Option`-returning big-endian reader.
///
/// Every read returns `None` rather than panicking or wrapping, so a
/// truncated or hostile class file degrades into "not found" instead of
/// a build-time crash. `#![forbid(unsafe_code)]` holds because there is
/// no unchecked slicing anywhere in this module.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn u1(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u2(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u4(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.take(count).map(|_| ())
    }

    /// Borrow `count` bytes, advancing the cursor, or `None` if the
    /// request runs past the end. `checked_add` guards the overflow
    /// that a malicious `u4` length could otherwise trigger.
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(count)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A method as described by a test, before it is encoded.
    struct MethodSpec {
        access_flags: u16,
        name: &'static str,
        descriptor: &'static str,
    }

    impl MethodSpec {
        const fn new(access_flags: u16, name: &'static str, descriptor: &'static str) -> Self {
            Self {
                access_flags,
                name,
                descriptor,
            }
        }
    }

    const ACC_PUBLIC_STATIC: u16 = ACC_PUBLIC | ACC_STATIC;

    /// Minimal constant-pool builder — enough to emit a parseable
    /// class file without pulling in a JVM.
    #[derive(Default)]
    struct PoolBuilder {
        entries: Vec<Vec<u8>>,
        utf8: std::collections::HashMap<String, u16>,
    }

    impl PoolBuilder {
        /// Add a `CONSTANT_Utf8`, reusing the index if the string is
        /// already present (real compilers dedupe; so does this).
        fn utf8(&mut self, value: &str) -> u16 {
            if let Some(&index) = self.utf8.get(value) {
                return index;
            }
            let index = self.entries.len() as u16 + 1;
            let mut entry = vec![1u8];
            entry.extend_from_slice(&(value.len() as u16).to_be_bytes());
            entry.extend_from_slice(value.as_bytes());
            self.entries.push(entry);
            self.utf8.insert(value.to_string(), index);
            index
        }

        fn class(&mut self, internal_name: &str) -> u16 {
            let name = self.utf8(internal_name);
            let index = self.entries.len() as u16 + 1;
            let mut entry = vec![7u8];
            entry.extend_from_slice(&name.to_be_bytes());
            self.entries.push(entry);
            index
        }

        /// Pad with `Long` constants, which consume two pool slots
        /// each. Used to prove the parser stays in sync across them.
        fn add_longs(&mut self, count: usize) {
            for _ in 0..count {
                let mut entry = vec![5u8];
                entry.extend_from_slice(&0u64.to_be_bytes());
                self.entries.push(entry);
                // A Long claims two slots, so the next index is
                // unusable. Recording it as an empty entry keeps the
                // vector's indices aligned with the real pool.
                self.entries.push(Vec::new());
            }
        }
    }

    /// Encode a syntactically valid class file carrying `methods`.
    fn build_class(internal_name: &str, methods: &[MethodSpec], extra_pool: usize) -> Vec<u8> {
        let mut pool = PoolBuilder::default();
        let this_class = pool.class(internal_name);
        let super_class = pool.class("java/lang/Object");

        // Encode the methods after the pool is otherwise settled so a
        // deduped Utf8 index is final before we read it back.
        let mut method_refs = Vec::new();
        for spec in methods {
            let name = pool.utf8(spec.name);
            let descriptor = pool.utf8(spec.descriptor);
            method_refs.push((name, descriptor));
        }
        pool.add_longs(extra_pool);

        let mut out = Vec::new();
        out.extend_from_slice(&CLASS_MAGIC.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&65u16.to_be_bytes()); // major (Java 21)
        out.extend_from_slice(&((pool.entries.len() + 1) as u16).to_be_bytes());
        for entry in &pool.entries {
            out.extend_from_slice(entry);
        }
        out.extend_from_slice(&ACC_PUBLIC.to_be_bytes()); // access_flags
        out.extend_from_slice(&this_class.to_be_bytes());
        out.extend_from_slice(&super_class.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // interfaces_count
        out.extend_from_slice(&0u16.to_be_bytes()); // fields_count
        out.extend_from_slice(&(method_refs.len() as u16).to_be_bytes());
        for (spec, (name, descriptor)) in methods.iter().zip(&method_refs) {
            out.extend_from_slice(&spec.access_flags.to_be_bytes());
            out.extend_from_slice(&name.to_be_bytes());
            out.extend_from_slice(&descriptor.to_be_bytes());
            out.extend_from_slice(&0u16.to_be_bytes()); // attributes_count
        }
        out.extend_from_slice(&0u16.to_be_bytes()); // class attributes_count
        out
    }

    fn class_with_main() -> Vec<u8> {
        build_class(
            "com/example/Main",
            &[MethodSpec::new(
                ACC_PUBLIC_STATIC,
                "main",
                "([Ljava/lang/String;)V",
            )],
            0,
        )
    }

    // ---- the happy path ----

    #[test]
    fn detects_public_static_main() {
        assert!(has_public_static_main(&class_with_main()));
    }

    #[test]
    fn detects_main_among_other_methods() {
        let bytes = build_class(
            "com/example/App",
            &[
                MethodSpec::new(ACC_PUBLIC, "<init>", "()V"),
                MethodSpec::new(ACC_PUBLIC, "start", "(Ljava/lang/Object;)V"),
                MethodSpec::new(ACC_PUBLIC_STATIC, "main", "([Ljava/lang/String;)V"),
            ],
            0,
        );
        assert!(has_public_static_main(&bytes));
    }

    /// Long and Double take two constant-pool slots each. If the
    /// parser didn't account for that it would desync and report a
    /// class that has no main as having one.
    #[test]
    fn stays_in_sync_across_wide_constants() {
        let bytes = build_class(
            "com/example/App",
            &[MethodSpec::new(
                ACC_PUBLIC_STATIC,
                "main",
                "([Ljava/lang/String;)V",
            )],
            3,
        );
        assert!(has_public_static_main(&bytes));
    }

    #[test]
    fn does_not_report_a_class_without_a_main() {
        let bytes = build_class(
            "com/example/HelloApplication",
            &[
                MethodSpec::new(ACC_PUBLIC, "<init>", "()V"),
                MethodSpec::new(ACC_PUBLIC, "start", "(Ljavafx/stage/Stage;)V"),
            ],
            0,
        );
        assert!(!has_public_static_main(&bytes));
    }

    // ---- near-misses: each must NOT be reported ----

    #[test]
    fn rejects_non_static_main() {
        let bytes = build_class(
            "com/example/App",
            &[MethodSpec::new(
                ACC_PUBLIC,
                "main",
                "([Ljava/lang/String;)V",
            )],
            0,
        );
        assert!(!has_public_static_main(&bytes));
    }

    #[test]
    fn rejects_non_public_main() {
        let bytes = build_class(
            "com/example/App",
            &[MethodSpec::new(
                ACC_STATIC,
                "main",
                "([Ljava/lang/String;)V",
            )],
            0,
        );
        assert!(!has_public_static_main(&bytes));
    }

    #[test]
    fn rejects_main_with_wrong_descriptor() {
        // A `main(int)` or `main()` is not a launcher entry point.
        for descriptor in ["()V", "(I)V", "([Ljava/lang/String;)I"] {
            let bytes = build_class(
                "com/example/App",
                &[MethodSpec::new(ACC_PUBLIC_STATIC, "main", descriptor)],
                0,
            );
            assert!(
                !has_public_static_main(&bytes),
                "descriptor {descriptor} should not count as an entry point"
            );
        }
    }

    #[test]
    fn rejects_differently_named_method() {
        let bytes = build_class(
            "com/example/App",
            &[MethodSpec::new(
                ACC_PUBLIC_STATIC,
                "run",
                "([Ljava/lang/String;)V",
            )],
            0,
        );
        assert!(!has_public_static_main(&bytes));
    }

    // ---- malformed input must never panic ----

    #[test]
    fn rejects_garbage() {
        assert!(!has_public_static_main(b""));
        assert!(!has_public_static_main(b"not a class file at all"));
        assert!(!has_public_static_main(&[0xCA, 0xFE]));
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut bytes = class_with_main();
        bytes[0] = 0x00;
        assert!(!has_public_static_main(&bytes));
    }

    /// Truncation is the normal case for an oversized class, so it has
    /// to be a quiet `false` rather than a panic.
    #[test]
    fn truncation_at_every_length_is_safe() {
        let bytes = class_with_main();
        for len in 0..bytes.len() {
            // Must not panic at any prefix length.
            let _ = has_public_static_main(&bytes[..len]);
        }
        assert!(has_public_static_main(&bytes));
    }

    /// A declared length that runs past the buffer must be caught by
    /// the `checked_add` / bounds check rather than wrapping.
    #[test]
    fn rejects_absurd_declared_length() {
        let mut bytes = class_with_main();
        // The first Utf8 length field sits right after magic/minor/
        // major/count (4+2+2+2) + tag byte.
        let offset = 4 + 2 + 2 + 2 + 1;
        bytes[offset..offset + 2].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(!has_public_static_main(&bytes));
    }

    // ---- entry-name mapping ----

    #[test]
    fn maps_entry_names_to_binary_names() {
        assert_eq!(
            class_name_from_entry("com/example/Main.class"),
            "com.example.Main"
        );
        assert_eq!(
            class_name_from_entry("com/example/Outer$Inner.class"),
            "com.example.Outer$Inner"
        );
        assert_eq!(class_name_from_entry("Main.class"), "Main");
    }
}
