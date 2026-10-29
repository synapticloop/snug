//! Shared fixtures for snug-cli's integration tests.
//!
//! The class-file writer lives here because several test files need one.
//! It used to live in `find_main.rs` alone, but the build now reads class
//! files properly — the entry-point check parses the superclass and the
//! method table — so a fixture that is only a magic number and a version
//! would make every test report an unreadable entry point.

#![allow(dead_code)]

/// A method to encode: `(access_flags, name, descriptor)`.
pub type MethodSpec = (u16, &'static str, &'static str);

/// JVM `ACC_PUBLIC` (JVMS §4.1).
pub const ACC_PUBLIC: u16 = 0x0001;

/// JVM `ACC_STATIC` (JVMS §4.1).
pub const ACC_STATIC: u16 = 0x0008;

/// `public static`.
pub const PUBLIC_STATIC: u16 = ACC_PUBLIC | ACC_STATIC;

/// The descriptor the JVM launcher accepts for an entry point.
pub const MAIN_DESCRIPTOR: &str = "([Ljava/lang/String;)V";

/// Encode a syntactically valid class file declaring `methods`.
///
/// Only what snug's class-file reader reads is emitted: magic, version, a
/// constant pool, no interfaces, no fields, the method table, and no
/// attributes. Method attribute blobs are omitted entirely, which is fine —
/// the parser skips them by length, and a count of zero is a valid encoding
/// of "none".
pub fn build_class(internal_name: &str, methods: &[MethodSpec]) -> Vec<u8> {
    build_class_with(internal_name, "java/lang/Object", 65, methods)
}

/// Encode a class file with an explicit superclass and class-file version.
///
/// `internal_name` and `super_class` are in internal form
/// (`com/example/Main`), which is what a class file stores.
pub fn build_class_with(
    internal_name: &str,
    super_class: &str,
    major: u16,
    methods: &[MethodSpec],
) -> Vec<u8> {
    let mut pool: Vec<Vec<u8>> = Vec::new();

    let utf8 = |pool: &mut Vec<Vec<u8>>, value: &str| -> u16 {
        let index = pool.len() as u16 + 1;
        let mut entry = vec![1u8];
        entry.extend_from_slice(&(value.len() as u16).to_be_bytes());
        entry.extend_from_slice(value.as_bytes());
        pool.push(entry);
        index
    };

    let this_name = utf8(&mut pool, internal_name);
    let this_class = {
        let index = pool.len() as u16 + 1;
        let mut entry = vec![7u8];
        entry.extend_from_slice(&this_name.to_be_bytes());
        pool.push(entry);
        index
    };
    let super_name = utf8(&mut pool, super_class);
    let super_class = {
        let index = pool.len() as u16 + 1;
        let mut entry = vec![7u8];
        entry.extend_from_slice(&super_name.to_be_bytes());
        pool.push(entry);
        index
    };

    let mut method_refs = Vec::new();
    for (_, name, descriptor) in methods {
        let name_index = utf8(&mut pool, name);
        let descriptor_index = utf8(&mut pool, descriptor);
        method_refs.push((name_index, descriptor_index));
    }

    let mut out = Vec::new();
    out.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    out.extend_from_slice(&major.to_be_bytes()); // major
    out.extend_from_slice(&((pool.len() + 1) as u16).to_be_bytes());
    for entry in &pool {
        out.extend_from_slice(entry);
    }
    out.extend_from_slice(&ACC_PUBLIC.to_be_bytes());
    out.extend_from_slice(&this_class.to_be_bytes());
    out.extend_from_slice(&super_class.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // interfaces_count
    out.extend_from_slice(&0u16.to_be_bytes()); // fields_count
    out.extend_from_slice(&(method_refs.len() as u16).to_be_bytes());
    for (spec, (name_index, descriptor_index)) in methods.iter().zip(&method_refs) {
        out.extend_from_slice(&spec.0.to_be_bytes());
        out.extend_from_slice(&name_index.to_be_bytes());
        out.extend_from_slice(&descriptor_index.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // attributes_count
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // class attributes_count
    out
}
