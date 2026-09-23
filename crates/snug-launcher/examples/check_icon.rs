//! Quick diagnostic: enumerate resources in the running process's
//! module via FindResourceW + EnumResourceNamesW. Confirms the
//! build.rs link-arg fix propagated to this example binary.

#![cfg(windows)]

fn main() {
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

    let hinst: HMODULE = unsafe { GetModuleHandleW(std::ptr::null()) };
    if hinst.is_null() {
        eprintln!("HMODULE is null");
        std::process::exit(1);
    }
    eprintln!("HMODULE = {:?}", hinst);

    // ---- Direct FindResourceW for "MAINICON" / RT_GROUP_ICON ----
    unsafe {
        use windows_sys::Win32::System::LibraryLoader::FindResourceW;
        let name_w = "MAINICON\0".encode_utf16().collect::<Vec<u16>>();
        let hres = FindResourceW(
            hinst,
            name_w.as_ptr(),
            // RT_GROUP_ICON = 14, cast to PCWSTR (high bit = 0 ⇒
            // integer resource id).
            14usize as *const u16,
        );
        eprintln!(
            "FindResourceW(\"MAINICON\", RT_GROUP_ICON=14) -> {:?}",
            hres
        );

        // Also try the integer-id form: Windows convention is id=1
        // when no name is associated.
        let hres_id1 = FindResourceW(
            hinst,
            1usize as *const u16,
            14usize as *const u16,
        );
        eprintln!(
            "FindResourceW(id=1, RT_GROUP_ICON=14) -> {:?}",
            hres_id1
        );
    }

    eprintln!("done");
}