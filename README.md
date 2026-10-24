<div align="center">

<img src="assets/snug-splash.png" alt="Snug" width="640">

# Snug

**build it. brand it. share it.**

Turn a runnable Java application into a Windows executable that people can double-click.

</div>

Snug wraps your Java application JAR (or a directory of JARs) in a native **Windows x64** `.exe`. It helps launch Java, can offer to download a compatible runtime, and lets you add your own icon, splash screen, and application details. It's designed for people who want to share a working Java application without asking others to run a JAR manually.


> **Development status:** Snug is under active development. Check [Releases](https://github.com/synapticloop/snug/releases) for available builds.
 

---

>See the completed documentation of what snug can do at [https://synapticloop.github.io/snug/](https://synapticloop.github.io/snug/)

---

## Your first Snug application

You need the Windows `snug.exe` builder and a runnable Java JAR. To try Snug without writing Java code, use the included [JavaFX demo JAR](assets/snug-javafx-demo-windows.jar).

1. Download `snug.exe` from [Releases](https://github.com/synapticloop/snug/releases) when a build is available.
2. Put `snug.exe` and `snug-javafx-demo-windows.jar` in a folder.
3. Open a terminal in that folder and run:

   ```powershell
   .\snug.exe .\snug-javafx-demo-windows.jar
   ```

4. Double-click the resulting `snug-javafx-demo-windows.exe`.

The executable contains your Java application and the native launcher. On first launch, Snug can offer to download a compatible Java runtime if one is not already available.

<div align="center">

<img src="assets/snug-javafx-demo-screenshot.png" alt="The Snug JavaFX demo application" width="302">

</div>

Already have your own runnable JAR? Use the same approach:

```powershell
.\snug.exe .\MyApplication.jar
```

By default, Snug creates `MyApplication.exe` alongside the input JAR. Existing `snug.options` files can supply additional settings.

## Why Snug?

- **Double-click to run.** Share a familiar Windows executable instead of instructions for running a JAR.
- **Java runtime assistance.** Snug looks for a compatible Java installation and can offer to download Eclipse Temurin when needed.
- **Your application, your branding.** Add an icon, splash screen, name, version, and Windows application details.
- **Straightforward sharing.** Keep small business and personal tools local, and choose how to distribute the executable.

## Make it your own

Start with the minimum command above, then add options as needed:

```powershell
.\snug.exe .\MyApplication.jar --name "My Application" --company "My Company" --icon .\icon.png --version 1.0.0
```

`--name` and `--company` are optional. Snug derives defaults from the input name.

For repeated builds, Snug supports `snug.options` configuration files. Run `snug.exe --init-options` for a starter file or `snug.exe --help` to see the full command-line reference. You can also customise runtime messages and supply translations using localisation files.

## What Snug is - and isn't

**Snug is an application builder, not an installer.** The resulting `.exe` is the application you share. Snug does not create Start menu entries, install your application into Program Files, or provide an uninstaller. Delete that executable and the application is no longer available from it, although cached files and downloaded Java runtimes may remain.

Snug does not automatically update your application, sign the executable, or publish it to an application store. You decide how to distribute new versions. Unsigned executables may trigger Windows security warnings or be subject to organisational policies.

## How it works

A Snug-built executable embeds your JAR or JARs and application configuration. When launched, it extracts the application into a per-user cache, finds a suitable Java runtime, and invokes Java through JNI. The application runs under its own `.exe` process name, rather than `javaw.exe`.

By default Snug requires Java 25 or newer; you can configure the minimum Java version with `--min-java`. If a compatible runtime is missing, Snug can offer to download Eclipse Temurin, verifying the downloaded archive with SHA-256.

## Learn more

- [Releases](https://github.com/synapticloop/snug/releases) - available builds.
- [Command-line options](https://github.com/synapticloop/snug/blob/main/crates/snug-cli/src/cli.rs) - source definitions; `snug.exe --help` is the authoritative reference for your build.
- [Localisation token reference](REPLACEMENT_TOKENS.md) - placeholders available in translated launcher messages.
- [Repository development notes](AGENTS.md) - implementation details and contributor context.
- *Say Hello to Snug* - the companion guide to building, branding, and sharing your first executable.

## Building from source

Snug is written in Rust. For Windows builds, use the repository's `scripts/windows/build.cmd` script, which builds the launcher and builder in the required order. See [development notes](AGENTS.md) for details.

The initial supported release targets **Windows x64**. Experimental code for other platforms may exist in the repository but is not part of this release.

## Licence

Dual-licensed under MIT or Apache-2.0, at your option.

---

<div align="center">

<img src="assets/snug-icon.png" alt="Snug mascot" width="180">

**Say Hello to Snug.**

</div>
