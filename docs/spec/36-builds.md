# Builds

This chapter defines the build inputs and the reproducibility of what the toolchain produces from them, the absence of build scripts, the absence of network use and the commands that fetch releases, the toolchain hash, the user directory with the build cache and its trust, stage comparison, and the `build` diagnostic family.

## Reproducibility

The **build inputs** of a command are exactly:

- the source files and [machine files](35-format.md#machine-files) of every workspace package the command reads;
- the [local store](29-identity-versioning.md#local-store);
- the source files and machine files of each package reached from those packages through a chain of path sources;
- the [toolchain hash](#toolchain-hash) of each package's [language version](37-evolution.md#language-versions);
- the command's arguments;
- in a [VCS workspace](31-trust.md#author-rule), the changes and authors that the author rule reads.

Two runs of a command with equal build inputs produce, byte for byte, the same code roots, [artefacts](30-artefacts.md#artefacts), objects from [lowering](30-artefacts.md#lowering) and [images](27-program-model.md#images) of the machine targets, the same diagnostics of checking and of the [release gate](31-trust.md#release-gate), in the same order, and the same outcomes of the tests the gate runs, except `test.timeout` and `test.emulator-unavailable` ([test runs](22-testing.md#test-runs)), which depend on the machine. The toolchain must give these outputs whatever the number of threads or processes, the order in which work is scheduled, the contents of the [build cache](#cache), the time, the machine, the user, the environment variables, the absolute path of the workspace, and the order and timestamps the file system reports. The one exception is `build.cache-exposed`, which depends on the owners and permissions in the [user directory](#cache). A host target's image is the same when, in addition, the [linker](27-program-model.md#images) that links it and every file and environment variable that linker reads are the same.

A [code root](30-artefacts.md#code-roots) depends only on its package's source files and machine files, the code roots of the other packages of its [dependency closure](27-program-model.md#final-builds), std included, and the [advisories](37-evolution.md#prover-monotonicity) naming its language version that the toolchain carries. Every conforming [implementation](00-charter.md#conformance) of that version carrying the same advisories gives the same code root.

## Build scripts

During checking and building, no code of any package runs except in the [interpreter's runs](21-interpreter.md#interpreter), and a final build starts no program other than the [linker](27-program-model.md#images) that links a host target's image. Data computed at build time is the value of a const item, computed by [const evaluation](21-interpreter.md#const-evaluation).

A symbolic link inside a [package](03-packages-modules.md#packages) directory is an error (`build.symbolic-link`) when no component of its path relative to the package directory starts with `.`, or when that path is `.nan`, `.nan/api` or the path of a machine file. The toolchain does not follow it, and reports it on the `package` line, naming that path.

The next example computes a checksum table at check time.

```nan
const CRC_POLYNOMIAL: u32 = 0xEDB8_8320

pub const CRC_TABLE: Array[u32, 256] = build_crc_table()

const assert CRC_TABLE[1] == 0x7707_3096

fn build_crc_table() -> Array[u32, 256] {
    var table = Array[u32, 256].filled(0)
    for index in 0..<256usize {
        var entry = u32.fit(index)
        for _ in 0..<8u8 {
            if (entry & 1) == 1 {
                entry = (entry >> 1) ^ CRC_POLYNOMIAL
            } else {
                entry = entry >> 1
            }
        }
        table[index] = entry
    }
    return table
} build_crc_table
```

## Network and fetching

No command of the toolchain opens a network connection.

**Fetching** is making available to the toolchain a release that a [lock](29-identity-versioning.md#lock) does not name: taking a [candidate](29-identity-versioning.md#resolution) into resolution, or appending a path release to the local store. Fetching reads only path sources and the local store.

- Only `nan dep add`, `nan dep sync` and `nan upgrade` without `--language`, which resolves as `nan dep sync` does, fetch.
- Every other command reaches a dependency other than std only through its locked release, read as [Lock](29-identity-versioning.md#lock) states: from source when that release is the current state of its path source, and otherwise from the source tree its local store entry holds.
- Reading the current state of a path source, or the releases the local store holds, to report `name.newer-release` is not fetching.

In the next example, `nan dep sync` in `wire` fetches the current state of `ring` as a path release, and `nan check` of `wire` then checks `ring` from source, since its locked release is the current state of `../ring`; after `ring` changes, `nan check` reads the locked release from its source tree in the local store until `nan dep sync` runs again.

```nan
// file: ring/package.nan
//! Fixed-capacity queues.

package ring {
    language 1
    targets any
} ring

// file: ring/ring.nan
//! Queue capacities.

pub const DEFAULT_CAPACITY: usize = 256

// file: wire/package.nan
//! Link message framing.

package wire {
    language 1
    targets any
    dep ring "../ring"
} wire

// file: wire/wire.nan
//! Frame slots sized by queue capacity.

pub const FRAME_SLOTS: usize = ring::DEFAULT_CAPACITY * 2
```

## Toolchain hash

The **toolchain hash** identifies a toolchain's behaviour for one language version: the [hash](29-identity-versioning.md#hash-values) of the [canonical NIR encoding](19-nir.md#encoding) of a record with exactly these keys:

| Key | Part | Content |
| --- | --- | --- |
| 1 | language version | a uvar |
| 2 | toolchain source tree hash | the hash of the encoding of a list of pairs, in byte order of path, one per file of the toolchain source tree: its path relative to the tree's root, with `/` separators, as a text, and its content, as bytes |

The **toolchain source tree** is the set of files the toolchain is built from: its source, the sources of the std packages it carries, the advisories it carries, and the files pinning the version of every library and compiler its build uses. A toolchain written in NaN is the image of a final build of its packages, whose code roots identify that source tree whichever toolchain built it ([reproducibility](#reproducibility)): its toolchain source tree hash is the hash of the encoding of the list of the code roots its image's [host ELF section](30-artefacts.md#host-elf-section) holds, in that section's order.

- A toolchain has one toolchain hash per language version it implements, fixed when it is built, and applies to each package the one of that package's language version.
- Two toolchains with equal toolchain hashes must produce equal outputs from equal build inputs.
- The toolchain hash is part of the key of every cache entry and is recorded in each artefact's attestation file.

## Cache

The **user directory** is the directory `.nan` in the home directory that the system's user database gives the user running the toolchain. It holds the **build cache** in its subdirectory `cache`, the local store in its subdirectory `store`, and the [secret keys](31-trust.md#principals) of principals in its subdirectory `keys`. The toolchain creates each of the first three that is missing, and `keys` when it first writes a secret key, accessible to the running user alone; it writes each secret key's file readable and writable by that user alone.

The build cache holds:

- **cache entries**, which are content-addressed: each is a value stored under the hash of its key, and the key holds the entry's kind, the toolchain hash and the hashes of every input its value depends on. They are the results of the [proof cache](20-prover.md#proof-cache) and of the [query engine](33-toolchain.md#query-engine), and may be the syntax of source files and the machine code of the functions a final build or a test image compiles. An entry's value equals what computing it again gives, so entries change no output that [Reproducibility](#reproducibility) lists.
- **cache records**, which later runs replace: the test pass records of [affected tests](22-testing.md#affected-tests), which decide only which tests `nan test --affected` and [resolution](29-identity-versioning.md#resolution) run, the key of the entry holding the query engine's results for the state of each closure last checked under each target in a workspace, and the key of the entry holding the machine code of the latest final build of each program and test image of each package under each target in a workspace, which decide nothing a command outputs.

Every process of the user shares the cache without locks.

- Each cache entry or cache record is one file, written whole under a new name in `cache` and then renamed into place. A cache entry is never rewritten; of two processes replacing one cache record, the later rename wins.
- Each file holds a checksum of its contents. The toolchain fixes the names and encoding of the files, and a file it cannot decode, or whose checksum does not match, is absent.
- Deleting any file of the cache, or the whole cache, at any time changes only what cache records decide.
- The toolchain uses a cache entry without checking it again, so the cache is trusted exactly as the installed toolchain is. Only the toolchain writes it, and reads none of the cache from another user's directory or from a location shared between users.
- Before reading the user directory, `cache`, `store`, `keys` or a file in them, the toolchain checks it. One that is a symbolic link, is not owned by the running user, or is writable by its group or by others, or for `keys` and its files accessible to them, is an error (`build.cache-exposed`), reported without a source line and naming its path relative to the home directory, as `~/.nan/...`. The command then ends, writing nothing to the user directory.

The toolchain fixes the names and encoding of the files in `store`. A command appending to the local store holds an exclusive advisory `flock` lock on `store/write` from reading the greatest publication index until its append completes, and writes each entry whole under a new name before renaming it into place, so a reader sees an entry entirely or not at all.

## Stage comparison

**Stage comparison** checks that the running toolchain and the toolchain that wrote a host target's image give the same code roots for that image's final closure. `nan build --compare-with <image>` runs it, `<image>` being the image's path. That image's [host ELF section](30-artefacts.md#host-elf-section) holds one artefact per package of the [final closure](27-program-model.md#final-builds) of the build that wrote it. The running toolchain compares their code roots, by package name, with those of the artefacts that its own final build of the package places in that section. It compares no attestation file, object or image. The comparison runs once the final build's checking step reports no error, and an error it reports ends the command before any image is written. `--compare-with` on a scope that does not hold exactly one program, or whose program does not declare a host target, is an error (`tool.usage`).

- An `<image>` that is not an ELF64 file holding exactly one `.nan` section whose artefacts decode is an error (`build.image-unreadable`), reported without a source line and naming the path.
- A package whose two code roots differ, or which only one of the two closures holds, is an error (`build.stage-mismatch`), naming the package and both code roots in short form, or `absent`. It is reported on the `package` line of a package checked from source, and otherwise without a source line.

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `build.cache-exposed` | error | [Cache](#cache) |
| `build.image-unreadable` | error | [Stage comparison](#stage-comparison) |
| `build.stage-mismatch` | error | [Stage comparison](#stage-comparison) |
| `build.symbolic-link` | error | [Build scripts](#build-scripts) |
