use std::os::unix::process::ExitStatusExt as _;

use super::*;

#[test]
fn a_run_ends_with_the_byte_written_to_the_exit_port_and_otherwise_with_70() {
    let exited = |code: i32| Ok(ExitStatus::from_raw(code << 8));
    assert_eq!(run_status(exited(1)), 0);
    assert_eq!(run_status(exited(141)), 70);
    assert_eq!(run_status(exited(255)), 127);
    assert_eq!(run_status(exited(0)), OTHER_END);
    assert_eq!(run_status(Ok(ExitStatus::from_raw(9))), OTHER_END);
    assert_eq!(run_status(Err(io::ErrorKind::NotFound.into())), OTHER_END);
}

/// The arguments of a machine run, separated by spaces.
fn written(arch: Arch, kvm: bool, kept: Option<bool>) -> String {
    base_args(arch, kvm, kept).join(" ")
}

#[test]
fn the_x86_64_machine_has_its_iommu_first_and_the_host_cpu_under_kvm() {
    let kvm = written(Arch::Amd64, true, None);
    let iommu = "-machine q35 -device intel-iommu,intremap=on,aw-bits=48 ";
    assert!(kvm.contains(&format!(
        "{iommu}-accel kvm,kernel-irqchip=split -cpu host,+invtsc "
    )));
    assert_eq!(kvm.find("-device"), kvm.find("-device intel-iommu"));
    let tcg = written(Arch::Amd64, false, None);
    assert!(tcg.contains(&format!("{iommu}-accel tcg -cpu max ")));
    assert!(tcg.contains("-device tpm-crb,tpmdev=tpm "));
}

#[test]
fn the_aarch64_machine_runs_under_tcg_with_an_smmu() {
    for kvm in [true, false] {
        let args = written(Arch::Aarch64, kvm, None);
        assert!(args.contains(
            "-machine virt,gic-version=3,its=on,iommu=smmuv3,highmem-ecam=off -accel tcg -cpu cortex-a72 "
        ));
        assert!(args.contains("-device tpm-tis-device,tpmdev=tpm "));
    }
}

#[test]
fn a_test_run_drops_its_writes_and_a_run_keeps_them() {
    let fresh = written(Arch::Amd64, true, None);
    for held in [
        "-serial stdio -serial null ",
        "-drive if=none,id=disk,format=raw,file=disk.raw,snapshot=on ",
        "-netdev user,id=net,restrict=on ",
    ] {
        assert!(fresh.contains(held), "{fresh} holds no {held}");
    }
    assert!(fresh.ends_with(" -no-reboot"));
    for resets in [false, true] {
        let kept = written(Arch::Aarch64, false, Some(resets));
        for held in [
            "-serial stdio -serial unix:console.sock,server=on,wait=off ",
            "-drive if=none,id=disk,format=raw,file=disk.raw ",
            "-netdev user,id=net",
        ] {
            assert!(kept.contains(held), "{kept} holds no {held}");
        }
        assert_eq!(kept.ends_with(" -no-reboot"), !resets);
    }
}

#[test]
fn every_virtio_pci_device_is_modern_and_behind_the_iommu() {
    for arch in [Arch::Amd64, Arch::Aarch64] {
        let args = base_args(arch, false, None);
        let devices: Vec<&String> = args
            .iter()
            .filter(|arg| arg.starts_with("virtio-"))
            .collect();
        assert!(devices.len() >= 2);
        for device in devices {
            assert!(
                !device.contains("-pci")
                    || device.ends_with(",iommu_platform=on,disable-legacy=on"),
                "{device}"
            );
        }
    }
}
