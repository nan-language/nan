use std::fs;

use crate::command::tests::run_in;
use crate::workspace::tests::Scratch;

const MANIFEST: &str = "//! Exits.\n\npackage exits {\n    language 1\n    targets x86_64-host\n    entry main\n} exits\n";

const SOURCE: &str = "//! Exits.\n\nfn main(take world: host::World) -> u8 fails task::Cancelled uses wait {\n    world.log.info(\"exiting\", value: 3u8)\n    return 3\n} main\n";

#[test]
fn stage_comparison_reports_each_package_whose_code_root_differs() {
    let scratch = Scratch::new(&[("exits/package.nan", MANIFEST), ("exits/exits.nan", SOURCE)]);
    let dir = &scratch.0;
    let built = run_in(dir, "build");
    assert_eq!(built.status, 0, "{}", built.output);
    fs::copy(
        dir.join(".nan/images/x86_64-host/exits"),
        dir.join("stage1"),
    )
    .expect("a copy");
    let same = run_in(dir, "build --compare-with stage1");
    assert_eq!(same.status, 0, "{}", same.output);
    let changed = SOURCE.replace("return 3", "return 4");
    fs::write(dir.join("exits/exits.nan"), changed).expect("a changed source");
    let differing = run_in(dir, "build --compare-with stage1").output;
    assert!(
        differing.contains("\nerror build.stage-mismatch exits/package.nan:3 exits\n"),
        "{differing}"
    );
    assert!(!differing.contains("\nwrote "), "{differing}");
}

const KERNEL: &str = "//! Spins.\n\nfn main(take k: boot::Kernel) -> Never limits stack 16_384 {\n    var spins: u64 = 0\n    loop {\n        spins = spins.wrapping_add(1)\n    }\n} main\n";

/// The bytes a bare image's loadable segments may span from its base: on the x86_64 machine, OVMF
/// keeps memory from `0x80_0000` that a UEFI loader placing the image must not overwrite.
const LOADABLE_SPAN: u64 = 0x70_0000;

#[test]
fn bare_image_segments_leave_the_boot_area_out() {
    let manifest = |name: &str, target: &str| {
        format!(
            "//! Spins.\n\npackage {name} {{\n    language 1\n    targets {target}\n    entry main\n}} {name}\n"
        )
    };
    let (x64, aarch64) = (
        manifest("x64", "x86_64-bare"),
        manifest("arm", "aarch64-bare"),
    );
    let scratch = Scratch::new(&[
        ("x64/package.nan", &x64),
        ("x64/x64.nan", KERNEL),
        ("arm/package.nan", &aarch64),
        ("arm/arm.nan", KERNEL),
    ]);
    let dir = &scratch.0;
    let built = run_in(dir, "build");
    assert_eq!(built.status, 0, "{}", built.output);
    for (image, base) in [
        ("x86_64-bare/x64.elf", 0x10_0000),
        ("aarch64-bare/arm.elf", 0x4020_0000),
    ] {
        let bytes = fs::read(dir.join(".nan/images").join(image)).expect("the image");
        let word = |at: usize, len: usize| {
            let mut value = [0; 8];
            value[..len].copy_from_slice(&bytes[at..at + len]);
            u64::from_le_bytes(value)
        };
        let (table, count) = (word(32, 8) as usize, word(56, 2) as usize);
        for header in (0..count).map(|index| table + 56 * index) {
            if word(header, 4) == 1 {
                let end = word(header + 24, 8) + word(header + 40, 8);
                assert!(end <= base + LOADABLE_SPAN, "{image} loads up to {end:#x}");
            }
        }
    }
}
