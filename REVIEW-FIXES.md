# snug — code review fix list

Read-only code review of the workspace at `c94b3c9` (5 crates, ~28.5k lines).
**29 findings** (28 from the review round, plus F-29 found while fixing F-05),
ordered by priority. macOS-only findings are excluded by request.

## How to use this file

Each fix has a stable ID (`F-01` … `F-28`). Edit only the **Status** line —
everything else is reference material for whoever picks it up.

| Status | Meaning |
|---|---|
| `TODO` | Not started (default for everything here) |
| `WIP` | In progress |
| `DONE` | Fixed and verified |
| `SKIP` | Consciously declined — add a note if you want |

To hand work back, just point at the ID, e.g. *"do F-07"*. Priority groups
run **P0 → P3**; within a group the order is the suggested sequence.

Nothing in this review has been changed except items explicitly marked `DONE`.

| Done | Item |
|---|---|
| ✅ `F-01` | `-o` overwriting the input JAR — guarded canonically, tested, verified end-to-end |
| ✅ `F-02` | `build-windows.cmd` step 8 — builds both dropper targets now |
| ✅ `F-03` | Stub-hash check — normalised via `scripts/pe-stable-hash.ps1`, verified reproducible |
| ✅ `F-04` | Dropper dialogs — caption/body swap fixed (F-04) and the overwrite prompt reworded |
| ✅ `F-05` | Cancel during verify/extract — now honoured, and no longer overruled by the worker |
| ✅ `F-06` | Windows JNI version follows the discovered JVM (ported from macOS) |
| ✅ `F-07` | A too-old JDK no longer aborts the scan (ported from macOS, plus its diagnostic kept) |
| ✅ `F-08` | `try_common` expanded to the JDKs inside each vendor directory |
| ✅ `F-09` | A candidate without `jvm.dll` is rejected before it reaches the loader |
| ✅ `F-29` | Concurrent JDK installs — staging + atomic publish + cross-process lock |

### Next action required before CI is green

`F-03` made the stub check work, and it immediately found real drift. Refresh the
committed stub and commit it:

```powershell
.\scripts\build-windows.cmd
git add bin\launcher-stub-windows-x86_64.exe
```

---

## P0 — data loss, silent failure, CI red

> IDs are stable, not positional: a finding added later keeps its number even
> if it lands in a different group. F-29 was found while fixing F-05.

### F-29 — Two snug apps downloading the same JDK corrupt each other's install
- **Status:** `DONE`
- **Found:** 2026-10-10, while walking F-05. Not in the original review round.
- **Why it matters:** `jdk_install_root()` is `%LOCALAPPDATA%\snug\jdk` — **machine-wide**, not per-app, because a JDK must outlive the per-app cache sweep (`platform/macos.rs:832-834`). Every path under it was derived from `min_java_major` alone, with no PID, nonce or lock. So two snug apps — or one app launched twice, which is what happens when someone impatiently re-clicks during a 200 MB download — collided on `<root>/<major>.zip.tmp` and `<root>/<major>/`.
- **Trigger:** no system Java + `--download-jdk` auto/force on two processes, both missing the cache.
- **Impact:** three distinct failures. (a) `run_one_install_attempt` opened with `remove_file(tmp_zip)` + `remove_dir_all(install_dir)`, so the second process **recursively deleted the tree the first was extracting into**, and both then wrote into it concurrently — either could report `Success` over a JDK missing most of its files. (b) The shared temp file interleaved, so SHA-256 failed and the user was told "SHA-256 mismatch" three times and sent to debug a network that was fine. (c) A half-extracted tree left at the canonical path was adopted by `find_cached_jdk` as a cache hit on the next launch.
- **Fixed:** three layers, because they solve different problems.
  1. **Staging + atomic publish.** The worker extracts into `<root>/.<major>.staging-<pid>/`; `publish()` validates via `find_java_home`, then renames staging onto `<root>/<major>` and rebases JAVA_HOME onto the published path. The canonical path now exists only if a rename put it there.
  2. **`find_cached_jdk` skips dot-prefixed entries**, so a mid-extraction staging tree can never be adopted as a cache hit.
  3. **Cross-process lock.** `InstallLock` on `<root>/<major>.lock`, acquired in `maybe_install` **before** the cache check — which is what makes the wait useful: if the process we waited for finished, the re-check becomes a cache hit and we never download at all. On Windows this is `LockFileEx`, which the kernel releases on crash, so there is no stale lock; other platforms degrade to `create_new` with an age-based takeover, documented as weaker.
  - `publish()` may only remove a pre-existing install because the caller holds that lock; the two layers depend on each other.
- **⚠️ Two bugs found in my own fix, both worth recording:**
  - `LockFileEx` **blocks** unless `LOCKFILE_FAIL_IMMEDIATELY` is passed. Without it the call never returns while a peer holds the lock, so the timeout below it was dead code and the launcher would hang forever — the exact failure mode this whole flow exists to avoid. Caught because the test binary hung, not by reading the code.
  - `LockFileEx` takes an `*mut OVERLAPPED`, which windows-sys gates behind the `Win32_System_IO` feature. That feature is now enabled in the workspace `Cargo.toml`.
- **Tests:** 4 new. `the_install_lock_actually_excludes` proves exclusion (a second short-wait acquire must fail, then succeed after `drop`), which is why `acquire_within` exists separately from `acquire`. Plus staging-hidden-from-cache-scan, publish-rebases-JAVA_HOME, publish-replaces-existing.
- **File:** `crates/snug-launcher/src/jdk_install.rs` (`InstallLock`, `staging_dir`, `publish`, `find_cached_jdk`), `crates/snug-launcher/src/platform/windows.rs:510-515`, `Cargo.toml`

### F-01 — `-o` pointing at the input JAR silently destroys it
- **Status:** `DONE`
- **Fixed:** 2026-10-10. New `build::guard_output_collision(&cli)`, called from `main.rs` immediately after the no-input check and before `build_payload`. Checks `jar`, `input`, `icon`, `splash` and `manifest` against the output by **canonicalised** path, not string. 4 new unit tests; `cargo test -p snug-cli` green (151 lib + 91 integration). Verified end-to-end: `snug demo.jar -o demo.jar` exits 1, prints the path and a `-o …demo.exe` hint, and the 10,254,138-byte JAR is byte-identical afterwards; an ordinary build still exits 0 and emits the EXE. Placement is deliberate — before `build_payload` so `--dry-run` reports the mistake too, even though it never reaches the write.
- **File:** `crates/snug-cli/src/build.rs:476-488`, `crates/snug-cli/src/cli.rs:159-163`
- **Trigger:** `snug app.jar -o app.jar`. `output_path` is `Some(p) => p.clone()` with no validation against `jar` / `input` / `icon` / `splash`, and `output` carries no `conflicts_with` (unlike `--input`, which has one). The JAR is fully read into memory before the write, so the write always succeeds.
- **Impact:** The fat JAR is truncated and overwritten by the launcher. Build exits **0** and prints `snug: built app.jar`. The user's only artefact is unrecoverable. Same for `-o icon.png` / `-o splash.png`.
- **Fix:** Reject when canonicalised `output` equals any input JAR, icon, splash or manifest path, before `build_payload`.

### F-02 — `build-windows.cmd` fails on a clean tree; step 8 never builds the dropper
- **Status:** `DONE`
- **Fixed:** 2026-10-10. Dropped `--bin stamp_dropper_icon` from both `BUILD_DROPPER_CMD` branches (`:173` zigbuild, `:202` native), so `-p snug-dropper` builds both bin targets. Added a comment at the native branch recording why, since the symmetry with step 7's `--bin snug_preview` is what made the omission easy to miss. Verified by moving both exes out of `target\release\` and re-running the new command verbatim: both reappeared (`snug-dropper.exe` 237,568 B, `stamp_dropper_icon.exe` 482,816 B).
- **File:** `scripts/build-windows.cmd:194` (and `:173`), failing at `:459-462`
- **Trigger:** `./scripts/build-windows.cmd --Clean`, or CI with a cold cargo cache. `BUILD_DROPPER_CMD` is `cargo build --release -p snug-dropper --bin stamp_dropper_icon` — it builds only the helper. No other step builds the `snug-dropper` binary. Step 7's `BUILD_PREVIEW_CMD` *does* carry `--bin snug_preview`; step 8 has no counterpart.
- **Impact:** `if not exist "!BUILT_DROPPER_EXE!"` exits 1, so `release\windows-x86_64\` never receives `Build with Snug.exe`, and CI's `upload-artifact` (`if-no-files-found: error`) fails too. Passes locally only because a stale `target\release\snug-dropper.exe` survives.
- **Fix:** Add `--bin snug-dropper` to both `BUILD_DROPPER_CMD` variants.

### F-03 — CI's "committed stub matches a fresh build" check can never pass
- **Status:** `DONE`
- **Fixed:** 2026-10-10. New `scripts/pe-stable-hash.ps1` with `Get-PeStableHash`, used by both `windows.yml` steps (`:97` records, `:128` compares). It zeroes four things: the PE `TimeDateStamp`, the optional-header `CheckSum`, the `IMAGE_DEBUG_DIRECTORY` table (mapped RVA→file offset through the section table — each of its 28-byte records carries its own copy of the link timestamp), and the RSDS CodeView record (located by signature: GUID + absolute PDB path). Script passes `Parser::ParseFile` clean.
- **⚠️ The committed stub is genuinely stale, so CI will now fail this step until you refresh it.** This is a true positive, not a regression: commit `ae9f18e` ("Substitute `{arch}` in the Windows progress window") touched `crates/snug-launcher/src` *after* the stub was committed in `934e179`. Run `scripts\build-windows.cmd` on Windows and commit the refreshed `bin\launcher-stub-windows-x86_64.exe`.
- **Evidence:** two release links 44 s apart with untouched source — raw SHA-256 `D4C61DA2…` vs `88AC68B5…`, differing in exactly 20 bytes: `TimeDateStamp` (1 byte, they differ only in the low byte at that spacing), the three `IMAGE_DEBUG_DIRECTORY` timestamps, and the 16-byte RSDS GUID. Normalised, both give `73AF36FB…`; the committed stub gives `88091C53…`, so the check still discriminates a genuinely different source state.
- **Note:** `/Brepro` on the MSVC link would make builds bit-reproducible and let this revert to a plain `Get-FileHash`, at the cost of a global link-flag change. Not done — the normaliser is contained and already verified.
- **File:** `.github/workflows/windows.yml:116-137`
- **Trigger:** Every CI run. MSVC `link.exe` stamps wall-clock time into `IMAGE_FILE_HEADER.TimeDateStamp` and there is no `/Brepro` anywhere. Verified: the committed stub carries `TimeDateStamp = 0x6AC427DF` = 2026-10-05 22:42:39 UTC, exactly equal to its file mtime.
- **Impact:** A freshly linked launcher never reproduces the committed SHA-256, so the step fails on every push and its own "commit the refreshed stub" remedy just relocates the failure. `build-windows.cmd` step 2 always leaves `bin\` dirty, and the failure is indistinguishable from the genuine source drift the step exists to catch.
- **Fix:** Hash the file with `TimeDateStamp` zeroed, or compare something link-stable (size + embedded payload).

### F-04 — Every dropper dialog shows title and body swapped
- **Status:** `DONE`
- **Fixed:** 2026-10-10. Renamed `message_box`'s parameters to `(caption, text, kind)` so the helper reads the way the dialog looks, keeping the `MessageBoxW` call itself in Win32 order (`lpText` first) — the swap now happens in exactly one place instead of at three call sites. `info` / `error` pass `TITLE` as the caption and the message as the body, which is what they always meant to do. `confirm` gained its own `caption` parameter so the overwrite prompt can name the file it is replacing.
- **Verified live** by launching the real binary and reading the dialog back over `WM_GETTEXT`:
  - overwrite → `CAPTION "Example Application Name.exe exists"` / body `"Overwrite?"` / `&Yes` `&No`
  - two items → `CAPTION "Build with Snug"` / body `"Only one jar is allowed. …"`
  - unsupported type → `CAPTION "Build with Snug"` / body `"Only .jar files and folders can be dropped on Build with Snug. …"`
- **Also changed at the author's request:** the overwrite prompt's wording. Title is now `<filename> exists` and the body is `Overwrite?`, replacing `<filename> already exists in <cwd>.\r\n\r\nReplace it?`. The output directory is no longer shown in the dialog (the title names the file, and the output path is deterministic from the input's parent) — reintroduce it in the body if that turns out to matter.
- **File:** `crates/snug-dropper/src/ui.rs:69-96`, caller at `crates/snug-dropper/src/main.rs:176-185`
- **Trigger:** Any drop at all — an unsupported file type, a build failure, the overwrite confirm.
- **Impact:** `message_box(text, caption, kind)` maps correctly onto `MessageBoxW(hwnd, lpText, lpCaption, …)`, but all three callers pass `TITLE` first. So `lpText = "Build with Snug"` and `lpCaption` = the error explanation, which then gets ellipsized in the title bar. The build-failure dialog — AGENTS.md calls it the user's *only* recourse on that path — loses its log path.
- **Fix:** Swap the two arguments at the three call sites (or rename the params to `caption, text`).

### F-05 — Cancelling during verify/extract launches the app anyway
- **Status:** `DONE`
- **Fixed:** 2026-10-10. Four changes, because the defect was wider than first described:
  1. `hash_file_sha256` and `extract_jdk_archive`/`extract_jdk_zip` now take `&AtomicBool` and consult it per 64 KB chunk / per zip entry, so verify and extract stop instead of running on after the window closed.
  2. New `claim_outcome(&shared, code)` uses `compare_exchange(0, code, …)` instead of `store`, so the worker *declares* a terminal outcome and never overrules one the UI already settled. `:1224`'s unconditional `done.store(1, …)` was the actual reported-outcome bug.
  3. New `Phase<T>` / `settle()` fold each phase result, so a cancellation landing *while a phase was failing* also reports `done == 3` rather than `2`.
  4. Tracing this surfaced a fourth manifestation: `download_to_disk` already signalled a cancel as `Err(JdkError::Download("cancelled by user"))`, so the worker's `Err` arm stored **2 (error)** — clicking Cancel mid-download raised "Download failed — Try again?", contradicting the doc at `jdk_install.rs:1231`. Fixed by the same `settle()` path.
- **Not a `JdkError` variant on purpose:** every `JdkError` is wired to a localisation key in `Display`, so a new variant would mean inventing user-facing text for something that is not a failure.
- **Tests:** 4 new. `a_worker_cannot_overrule_a_cancel_it_races_against` is the regression test for the headline bug; `a_cancel_during_a_failing_phase_reports_cancelled_not_failed` and `a_genuine_failure_still_reports_failed` are the two halves of the preference; `hashing_stops_promptly_when_cancelled` covers the per-chunk latch.
- **File:** `crates/snug-launcher/src/jdk_install.rs:1041`, `:1120`, plus the phase functions
- **Trigger:** Download finishes, user clicks Cancel during phase 1 (SHA-256) or phase 2 (extract) — both live for tens of seconds on a ~190 MB archive.
- **Impact:** `shared.cancel` is checked only inside `download_to_disk`'s read loop and once at `:1041`. Phases 1-2 never check it, then `:1120` runs `shared.done.store(1, …)` **overwriting** the `done = 3` the UI wrote. `run_one_install_attempt` reads `done == 1` → `Success(home)` (`:1402`). Three comments assert the opposite of what the code does (`jdk_install.rs:358-367`, `:1394-1397`, `progress_window.rs:968-969`). Cross-platform.
- **Fix:** Check `cancel` between phases 1 and 2, and make the terminal `done.store(1)` respect it.

---

## P1 — Windows runtime correctness (the shipping platform)

### F-06 — Windows still pins `JNIVersion::V21`
- **Status:** `DONE`
- **Fixed:** 2026-10-10. Ported `jni_version_for` from `platform/macos.rs:875-891` (added in `8c32c1d`, never backported) and wired it at `windows.rs:308`, which now reads the discovered major via `read_java_major` and logs the JNI level it is requesting.
- **Tests:** `jni_version_never_exceeds_the_discovered_jvm` — the two-sided invariant: never below the VM's floor, never above the spec's ceiling (`JNI_VERSION_21` is the highest `JNI_CreateJavaVM` accepts; there is no 22/23/24/25).
- **Known coverage limit:** this test pins the *function*, not the wiring at `:308`. Re-pointing `create_vm_args.version(...)` back at a constant would not fail it. Asserting the wiring needs the launch path and a real JVM, so it stays a manual check — worth knowing rather than assuming.
- **File:** `crates/snug-launcher/src/platform/windows.rs:308`, `:885-914`
- **Trigger:** `snug App.jar -o App.exe --min-java 17`. Discovery accepts a Java 17, then `InitArgsBuilder` asks for a JNI 21 entry point that isn't in it.
- **Impact:** `JavaVM::with_libjvm` fails with a useless `JNI_CreateJavaVM failed: …` on a machine discovery just reported as conforming.
- **Fix:** Port `jni_version_for` from `platform/macos.rs:859-891` (self-contained, already has 9 tests). **The macOS twin was fixed and Windows was not.**

### F-07 — One too-old JDK aborts the entire Windows discovery scan
- **Status:** `DONE`
- **Fixed:** 2026-10-10. `check_candidate` returns `Err(JvmTooOld)` and every call site propagated it with `?`, so the first too-old candidate ended discovery. Ported `scan_candidate` from `macos.rs:977-992` and switched the three *survey* steps onto it — `PATH`, the registry walk, and `try_common`. The first three steps (`--jvm-home`, `JAVA_HOME`, `JDK_HOME`) deliberately keep `check_candidate`, because those are explicit choices and "you have Java 11, you need 25" is the answer the user wants rather than something to scan past.
- **⚠️ Not a blind port — the macOS twin has a gap.** `macos.rs` records `too_old` and then returns `Ok(None)` at `:1068` without ever reading it, so the "you have Java 11, you need 25" message is silently lost; its test only asserts the *recording*. Windows keeps the diagnostic: `discover_jvm` returns `JvmTooOld` at the end if the scan found nothing and remembered something. Windows is therefore strictly better than the reference it was copied from. **The macOS gap is still there** — worth a follow-up if macOS is ever in scope.
- **Tests:** 3 new, and mutation-verified. Reverting `scan_candidate` to propagate the error makes exactly the two F-07 tests fail, which is what makes them regression tests rather than assertions of current behaviour. `an_explicit_choice_of_a_too_old_jdk_is_still_an_error` is the deliberate complement: it proves `check_candidate` *does* reject the old JDK, which is why the scan must not use it.
- **File:** `crates/snug-launcher/src/platform/windows.rs:695-712`, `:627-693`
- **Trigger:** Oracle JDK 11 registered under `JavaSoft\JDK` (scanned first, `:672`) plus Adoptium 25. `check_candidate` returns `Err(LauncherError::JvmTooOld)` and every call site propagates with `?`.
- **Impact:** Discovery stops at the old JDK; the user is told to fix `JAVA_HOME`/`PATH` or uninstall, despite a conforming Java 25 being installed. Commit `8c32c1d` fixed exactly this for macOS via `scan_candidate` (`macos.rs:977-992`) and never touched the Windows twin.
- **Fix:** Port `scan_candidate` to `windows.rs`; step over too-old candidates, reporting the remembered one only if nothing qualifies.

### F-08 — `try_common` can never match — the scan stops at the parent directory
- **Status:** `DONE`
- **Fixed:** 2026-10-10. `common_install_paths()` returned *vendor* directories (`%ProgramFiles%\Java`, `%ProgramFiles%\Eclipse Adoptium`) while `check_candidate` requires `bin\java.exe` **directly** beneath, so every candidate failed `is_dir` and the step was dead code. Split the expansion into `expand_install_root`, which returns the root itself (a flat install is legal and costs one `is_dir`) plus each immediate child directory, sorted for determinism.
- **Tests:** `a_vendor_directory_expands_to_the_jdks_inside_it` and `an_empty_vendor_directory_still_yields_itself`. Mutation-verified: reverting `expand_install_root` to `vec![root]` fails the first and nothing else.
- **File:** `crates/snug-launcher/src/platform/windows.rs`
- **Trigger:** A JDK under `C:\Program Files\Java\jdk-25.0.1\` on a machine with no `JAVA_HOME` and no registry entry for that vendor.
- **Impact:** `common_install_paths()` returns *vendor* directories, but `check_candidate` requires `bin\java.exe` **directly** beneath, so every candidate fails `is_dir`. The step is dead code despite `snug-format/src/config.rs:223-225` promising `C:\Program Files\Java\…`.
- **Fix:** Enumerate child directories of each vendor root (as the macOS version does) and test each as a candidate.

### F-09 — Discovery accepts a candidate the loader can't use
- **Status:** `DONE`
- **Fixed:** 2026-10-10. `check_candidate` asked only for `java.exe`, so a JRE-only or pruned install was returned as *the* discovery result and the run then died at `locate_jvm_dll` with a fatal `LibraryLoad` — never trying the registry or common-location candidates that would have worked. The check now also requires `locate_jvm_dll` to succeed, which is what turns that fatal end into "try the next one".
- **Tests:** `a_jre_without_a_jvm_dll_is_not_accepted_as_a_candidate`, and `a_jvm_dll_anywhere_the_loader_looks_is_enough`, which walks all three layouts `locate_jvm_dll` probes (`bin\server`, `bin\client`, `bin\`) so the gate and the loader cannot drift apart. Mutation-verified: removing the gate fails only the first test.
- **⚠️ Side effect worth knowing:** this changed the meaning of the `fake_jdk` test fixture, which planted `java.exe` but no `jvm.dll` — exactly the shape this finding rejects. Had the fixture not been updated, the F-07 tests would have started failing for the *wrong* reason while still passing. That is a useful reminder that a fixture can silently stop testing anything once a gate tightens.
- **File:** `crates/snug-launcher/src/platform/windows.rs`
- **Trigger:** `JAVA_HOME` pointing at a JRE-only or pruned JDK — has `bin\java.exe` and a new enough version, but no `bin\server\jvm.dll`.
- **Impact:** Discovery accepts it and returns, so the scan stops. The failure only surfaces afterwards as a hard `LibraryLoad` error at `:281-290`, with no attempt to fall back to the registry or common-location candidates that would have worked.
- **Fix:** Add the `locate_jvm_dll` existence test to the candidate check.

### F-10 — Cache extraction isn't atomic, and a truncated JAR is never reclaimed
- **Status:** `TODO`
- **File:** `crates/snug-launcher/src/platform/windows.rs:616-625`, `crates/snug-launcher/src/cache.rs:261-269`, `:293-297`
- **Trigger:** An interrupted write, a full disk, or two concurrent launches of the same build (double-click, or an app that relaunches itself).
- **Impact:** `ensure_cached` is `exists()` → `fs::write()` (O_TRUNC). A short `app.jar` passes the only check forever — the sweep reclaims directories with *no* `app.jar` (`:261-269`), and the current build is never evicted (`:293-297`). The app stays broken with `ZipException` / "no Main-Class found" until the user deletes the cache by hand.
- **Fix:** Write to a sibling temp file and `fs::rename` into place; treat an existing entry whose length ≠ `bytes.len()` as absent.

### F-11 — The Windows EXE is written non-atomically over the previous one
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/build.rs:551-553` → `vendor/editpe/src/image.rs:276-278`
- **Trigger:** Disk full, an AV/file-indexer holding a handle, or Ctrl-C during the write.
- **Impact:** `Image::write_file` is `std::fs::write` (`File::create` truncate + `write_all`), so a failed build replaces a good `App.exe` with a truncated one that won't load. `create_dir_all` at `:525-530` makes it worse for nested outputs: the parent survives, the file doesn't. *(Distinct from F-10, which is cache extraction in the launcher — this is the builder's own output.)*
- **Fix:** Write to `<output>.tmp` in the same directory, then `fs::rename` over the target.

### F-12 — The documented `\#` escape is dead
- **Status:** `TODO`
- **File:** `crates/snug-format/src/localization.rs:91` (`strip_comment`), `:116` (`unescape`), `:151-160`
- **Trigger:** Any bundle value containing a `#`, written exactly as the format instructs (`crates/snug-format/assets/snug-localisations.en.txt:20`): `err.foo = Error \#42`.
- **Impact:** `strip_comment` runs first and does a bare `line.find('#')`, matching the *escaped* hash and cutting the line. The escape never reaches `unescape`. The value silently becomes `Error \` in a user-facing dialog — text lost, stray backslash added, no parse error, and the key exists so the missing-key check can't flag it. The `unescape_handles_common_sequences` test passes only because it calls `unescape` in isolation, bypassing the real order. Confirmed by three independent reviewers.
- **Fix:** Make `strip_comment` find the first *unescaped* `#`.

---

## P2 — user-facing correctness and diagnostics

### F-13 — Multi-release JAR entries reported as launchable entry points
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/classfile.rs:109`, `:136`, `:289-294`
- **Trigger:** Any JAR with `Multi-Release: true`.
- **Impact:** The skip matches only the exact name `module-info.class`, so `META-INF/versions/<n>/…` entries are scanned and `/` → `.`, giving `--find-main` output like `META-INF.versions.17.com.example.Main`. That is the diagnostic users are told to copy into `--main-class`, producing a launcher that cannot start.
- **Fix:** Skip entries under `META-INF/versions/` in `find_main_classes`.

### F-14 — A non-UTF-8 manifest aborts the entire build
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/manifest.rs:45-48`, propagated at `crates/snug-cli/src/build.rs:52-53`
- **Trigger:** A JAR whose `META-INF/MANIFEST.MF` contains any non-UTF-8 byte (e.g. a Latin-1 `Name:` section).
- **Impact:** `read_to_string` fails and becomes a hard `?`, so `snug` refuses a JAR that `java -jar` runs happily, pointing at the JAR rather than the one bad attribute. One call away, `survey_main_classes` swallows the identical failure, so `--find-main` reports "no main class" instead.
- **Fix:** `String::from_utf8_lossy` the bytes — the parser only ever looks for an ASCII `Main-Class:` header.

### F-15 — The terminal JDK-failure dialog renders no error text
- **Status:** `TODO`
- **File:** `crates/snug-format/assets/snug-localisations.en.txt:215`, `crates/snug-launcher/src/jdk_install.rs:1263-1269`, `crates/snug-launcher/src/error_window.rs:116-118`, `:148`
- **Trigger:** All `MAX_DOWNLOAD_ATTEMPTS` (3) fail — DNS, TLS, proxy interception, or SHA-256 mismatch.
- **Impact:** `jdk_install.failure.content` is **empty**, so `fill("")` returns `""` and the `{error}` substitution is dead code. `error_window.rs:116-118` states content is "always caller-supplied" and `:148` passes it verbatim, while heading/subheading/info/button all get empty-fallbacks — the fallback `dialogs.rs:337-339` and AGENTS.md both claim exists. The user gets "All download attempts were exhausted" and no cause, losing the diagnostic that distinguishes a network blip from a corrupt download.
- **Fix:** Give `jdk_install.failure.content` a real template (mirroring `jdk_install.retry.content`), or add the missing content default in `error_window::show`.

### F-16 — Bare `--download-jdk` swallows the next token
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/cli.rs:308-317`, `crates/snug-cli/src/options_file.rs:532-535`, `:604-606`, `:687-691`
- **Trigger:** `snug app.jar --download-jdk --name "CLI Name"`.
- **Impact:** The arg is `require_equals = true, num_args = 0..=1`, so clap never consumes a following token — but `FlagSpec` derives `takes_value` from `ArgAction::takes_values()` and sets `skip_next`, eating `--name`. The file's `--name` is then not stripped and clap aborts with "the argument '--name <NAME>' cannot be used multiple times" — exactly what `FlagSpec` was written to eliminate. The attached `--download-jdk=auto` in your own `snug.options` is unaffected; the bare spelling is what `cli.rs:296-298` and `assets/snug.options.example:171` document.
- **Fix:** Gate the `takes_value` insert on `!arg.is_require_equals_set()`.

### F-17 — A UTF-8 BOM turns the first option into an unknown argument
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/options_file.rs:220-243`
- **Trigger:** A `snug.options` saved with a BOM — PowerShell 5.1 `Out-File` / `Set-Content -Encoding UTF8`, or many Windows editors.
- **Impact:** `read_to_string` keeps U+FEFF and `str::trim()` does not remove it (it isn't Unicode whitespace), so the first token becomes `"\u{feff}--name"`. `long_flag_name` fails to match, clap reports `unexpected argument '﻿--name' found` with an invisible character, and `cli_has_positional` (`:650`) counts it as a positional so the file's `--input` is stripped for the wrong reason — one bad byte, two misleading signals.
- **Fix:** Strip a leading `\u{feff}` from `content` in `load`.

### F-18 — `SnugPayload.icon` is a dead field that doubles every EXE's icon bytes
- **Status:** `TODO`
- **File:** `crates/snug-format/src/payload.rs:42-50`, populated at `crates/snug-cli/src/build.rs:60-63`, stamped from `crates/snug-cli/src/resources.rs:50`
- **Trigger:** Any `--icon` build.
- **Impact:** The builder reads the icon into the payload, and `resources.rs:50` re-reads `cli.icon` from disk to stamp the PE. Nothing reads `payload.icon` anywhere. Postcard is uncompressed, so every EXE grows by the full icon size (a multi-resolution `.ico` is easily hundreds of KB) for bytes never consumed. The field's doc comment justifies itself with the opposite of what the code does.
- **Fix:** Either stamp from `payload.icon`, or drop the field (safe — it's `#[serde(default)]` and read by no launcher) and correct the doc.

### F-19 — GUI-subsystem guarantee lives only in a path nothing calls
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/resources.rs:139-141`, `crates/snug-cli/src/stub.rs:41-57`
- **Trigger:** Any future change that drops `windows_subsystem` in `crates/snug-launcher/src/main.rs:13-16`, or a stub built with `debug_assertions` on.
- **Impact:** `image.set_subsystem(IMAGE_SUBSYSTEM_WINDOWS_GUI)` — commented "Defensive: ensure the produced binary stays a Windows GUI app" — is reachable only from `tests/editpe_roundtrip.rs:48,138`; production `build_exe` calls `plan.apply()` and never stamps it. And `stub.rs:41-57` is named `stub_is_a_pe32_plus_gui_exe` while asserting only `MZ` and `PE\0\0` — neither machine nor subsystem. Not a live bug today, but the guard is a test and the test over-promises.
- **Fix:** Stamp the subsystem in `build_exe`, or make the stub test assert `IMAGE_FILE_MACHINE_AMD64` and `Subsystem == 2`.

### F-20 — Every launcher error re-decodes the whole payload for one URL
- **Status:** `TODO`
- **File:** `crates/snug-launcher/src/main.rs:107-116`
- **Trigger:** Any post-decode failure — `JvmNotFound`, `JavaException`, `MainClassNotFound`.
- **Impact:** `show_launcher_error` re-runs `find_in_file` + `decode`, copying every JAR byte into new `Vec`s, purely to read `update_check_url`. On a 300 MB fat JAR the error path re-reads 300 MB and roughly doubles peak memory — on exactly the `JvmNotFound` path where the machine is already struggling. The payload was already decoded in `run()` at `:43`.
- **Fix:** Carry the URL (or the decoded payload) out of `run()` into the error window.

---

## P3 — lower severity / polish

### F-21 — A `--options` with no value is silently discarded
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/options_file.rs:204-206`, `:448-450`
- **Trigger:** `snug app.jar --options` (flag last, no path). `find_options_flag` returns `None`, so `resolve_all` falls back to the ambient default names and `strip_options_flag` then deletes the dangling token.
- **Impact:** clap never raises "a value is required", so the build proceeds on whatever `snug.options` is lying around, exit 0 — the opposite of what `--options` exists to guarantee. The typo `--options --name Foo` is likewise mishandled (`--name` taken as the path).
- **Fix:** Treat `--options` with no following value as a parse error.

### F-22 — Meta-flags written *into* an options file are silently ignored
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/options_file.rs:384-458`, `crates/snug-cli/src/main.rs:40`
- **Trigger:** `snug.options` containing `--options other.options` — the flag is advertised in the shipped template at `assets/snug.options.example:240`.
- **Impact:** `strip_options_flag` runs only over the CLI portion and `collect_cli_flags` deliberately never records `options` in `seen`, so the file's pair reaches clap, sets `cli.options`, and nothing ever loads it. Same for `--init-options` / `--init-localizations` from a file: `is_init_flag` scans raw argv only, so it parses, builds, and never scaffolds.
- **Fix:** Strip or explicitly reject `--options` and the `--init-*` family in `strip_overridden`/`merge`.

### F-23 — Malformed `--manifest` XML is embedded verbatim
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/resources.rs:93-105`
- **Trigger:** `--manifest broken.xml` with an XML syntax error (unclosed tag, stray `<`, bad attribute).
- **Impact:** `editpe` 0.2.4's `set_manifest` validates only resource-table structure, stores the string as opaque bytes and returns `Ok`. The build reports success and the defect surfaces only on the target machine as an SxS activation failure — the app never starts, with no build-time signal. *(Verified against the vendored `editpe` source, not reproduced locally.)*
- **Fix:** Reject non-well-formed XML at the CLI boundary before calling `set_manifest`.

### F-24 — The dropper shifts a multi-segment relative input
- **Status:** `TODO`
- **File:** `crates/snug-dropper/src/build.rs:59-78`
- **Trigger:** `Build with Snug.exe build\libs` from a terminal or a shortcut argument whose path is relative with more than one segment.
- **Impact:** `cwd` is the input's parent while the input itself passes through verbatim, so `build\libs` resolves to `build\build\libs`; the same one-level shift applies to `-o`. Windows drag-and-drop always passes absolute paths, so this is dev/shortcut-only.
- **Fix:** Resolve `input` to an absolute path before computing `cwd` and `output`.

### F-25 — `payload_len` is an unchecked narrowing cast
- **Status:** `TODO`
- **File:** `crates/snug-format/src/embedded.rs:72`
- **Trigger:** An input whose encoded payload reaches 4 GiB (a fat JAR bundling a runtime or a large model).
- **Impact:** `payload_bytes.len() as u32` wraps, writing a header whose `payload_len` disagrees with the bytes actually written while `payload_crc32` still covers the full buffer. The launcher then fails on `Truncated`/`BadCrc32` and the artefact is dead on arrival, with a message pointing at the launcher rather than the build.
- **Fix:** `u32::try_from(payload_bytes.len())` and fail the build with the actual size.

### F-26 — A wrapped `Main-Class` continuation yields "no main class" silently
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/manifest.rs:89-108`
- **Trigger:** A `MANIFEST.MF` whose `Main-Class` value hits the 72-byte line limit, so a conforming writer emits an indented continuation.
- **Impact:** `parse_main_class` returns the truncated first line's value, or `None` if the value is entirely on the continuation. Either way the build emits no warning — `warn_on_ambiguous_manifests` only fires for multiple *declaring JARs*. The doc at `:91-93` asserting "none are legal for `Main-Class` anyway" is wrong.
- **Fix:** Join indented continuation lines before splitting on `:`.

### F-27 — Each build serialises the whole payload three times
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/main.rs:121-122`, `crates/snug-cli/src/build.rs:522-523`
- **Trigger:** Any normal build.
- **Impact:** `main.rs:122` builds a `SnugEmbedded` (full postcard encode for len + CRC), then `build.rs:522` builds a *second* from `payload.clone()` — a full copy of every JAR byte plus another encode — and `:523` encodes a third time. Peak memory runs at roughly 3-4× the JAR size on the 200 MB case the code itself cites.
- **Fix:** Pass the `&SnugEmbedded` already built in `main.rs` into `build_exe`.

### F-28 — The exe-dir options tier doesn't carry relative paths, despite the documented promise
- **Status:** `TODO`
- **File:** `crates/snug-cli/src/options_file.rs:34-39` (doc) vs `:161-187` (behaviour)
- **Trigger:** Shipping `snug.exe` beside a `snug.options` naming a relative input (as the repo's own does with `assets/…`) and invoking `snug` from a different CWD.
- **Impact:** `resolve_all` finds the file in exe-dir, but the tokens merge into argv and every path in them resolves against the CWD, so the documented promise "carries its defaults wherever it is invoked from" holds for scalar flags only. The failure is a confusing `stat-ing input app.jar: system cannot find the file`, from a file the tool itself located and announced.
- **Fix:** Resolve path-valued tokens in an exe-dir file against that file's directory, or narrow the doc claim.

---

## Verified sound — don't re-audit these

Payload self-scan and CRC-before-decode ordering (`snug-format/src/codec.rs:52-94`);
classfile parsing is panic-free (`checked_add` + safe slicing, bounded 64 KB window with an
honest `oversized` note surfaced to the user); `build.rs` completes all encode/stamp work in
memory before a single write, so a stamp failure cannot truncate an existing EXE;
classpath built via `std::env::join_paths` and passed as one JNI option string, so
`Program Files` / `%LOCALAPPDATA%` spaces are safe; `budget_for` overflow-safe
(`saturating_mul` + `clamp`), current build never evicted; registry enumeration respects
`RegEnumKeyExW`/`RegQueryValueExW` lengths and validates `JavaHome` with `is_dir`;
resource stamping never silently swallowed (icon / manifest / version all propagate `?`);
RT_RCDATA lookup is by resource name, so a false-positive `SNUGEMBD` in the stub cannot mislead it;
dropper uses `Command::args()` argv throughout — no shell, no injection possible;
`decide.rs` classification consistent across double-click and drag-and-drop;
`merge` builds `FlagSpec` once per call and walks strictly high-to-low;
placeholder substitution is `str::replace`-based, never `format!`, so no user-facing string can panic;
malformed options files fail loudly with a line number;
`build-macos.sh` / `build-macos-demo.sh` gate staging on `[[ -d "$DEMO_APP" ]]`, `lipo` arch and
`otool minos` before copying anything.

## Stale docs spotted in passing

- `AGENTS.md:1791-1792` still says "Cross-platform launcher (mac `.app`, Linux ELF) —
  explicitly NOT in scope. Snug is Windows-only by design", which contradicts slices 11-13
  and most of the "Build host" section.