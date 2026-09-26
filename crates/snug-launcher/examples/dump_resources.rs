//! Diagnostic: dump the icon resource table of an EXE.
//! Used to verify which RT_ICON entries MAINICON references and to
//! detect orphan icons left behind by a previous stamp.
//!
//! Usage:
//!     cargo run --example dump_resources -- [path-to-exe]
//!
//! Defaults to `target/debug/snug_preview.exe` if no argument is
//! given. Particularly useful after a `cargo run --bin
//! stamp_preview_icon` to confirm the resource tree is clean (no
//! RT_ICON orphans accumulating from earlier stamp runs).

fn main() {
    let exe = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/debug/snug_preview.exe".to_string());

    let image = editpe::Image::parse_file(&exe).expect("parse_file");
    let Some(resources) = image.resource_directory() else {
        println!("<no resource directory>");
        return;
    };

    println!("Resource directory of {exe}:");

    // RT_ICON = 3
    let icon_table_name = editpe::ResourceEntryName::ID(3);
    println!("\n  RT_ICON entries (top-level ID(3)):");
    if let Some(editpe::ResourceEntry::Table(icon_table)) = resources.root().get(&icon_table_name) {
        let mut ids: Vec<u16> = icon_table
            .entries()
            .into_iter()
            .filter_map(|n| match n {
                editpe::ResourceEntryName::ID(id) => Some(*id as u16),
                _ => None,
            })
            .collect();
        ids.sort();
        for id in ids {
            println!("    Icon ID: {id}");
        }
    } else {
        println!("    (none)");
    }

    // RT_GROUP_ICON = 14
    let group_table_name = editpe::ResourceEntryName::ID(14);
    println!("\n  RT_GROUP_ICON entries (top-level ID(14)):");
    let Some(editpe::ResourceEntry::Table(group_table)) =
        resources.root().get(&group_table_name)
    else {
        println!("    (none)");
        return;
    };
    for group_name in group_table.entries() {
        println!("    Group: {group_name:?}");
        let Some(editpe::ResourceEntry::Table(inner)) = group_table.get(group_name) else {
            continue;
        };
        for lang_name in inner.entries() {
            let Some(editpe::ResourceEntry::Data(data)) = inner.get(lang_name) else {
                continue;
            };
            let bytes = data.data();
            if bytes.len() < 6 {
                continue;
            }
            let count = u16::from_le_bytes([bytes[4], bytes[5]]);
            println!("      lang={lang_name:?} icon_count={count}");
            for i in 0..count {
                let base = 6 + i as usize * 14;
                if bytes.len() < base + 14 {
                    break;
                }
                let id = u16::from_le_bytes([bytes[base + 12], bytes[base + 13]]);
                let dim_byte = |d: u8| if d == 0 { 256u16 } else { d as u16 };
                let w = dim_byte(bytes[base]);
                let h = dim_byte(bytes[base + 1]);
                println!(
                    "        #{i}: id={id}, {w}x{h}, {} bytes",
                    u32::from_le_bytes([
                        bytes[base + 8],
                        bytes[base + 9],
                        bytes[base + 10],
                        bytes[base + 11]
                    ])
                );
            }
        }
    }
}
