//! The facts of the operations std declares without a body, which the toolchain supplies: those
//! their owning chapters state for named operations, and those growth, interior mutability,
//! capabilities and floats give the others.

use std::collections::BTreeSet;

use super::Checker;
use super::db::{ItemId, TypeKey};
use super::digest::Digest;
use super::reversibility;
use super::sig::FnSig;
use super::ty::Ty;

/// The supplied operations that draw memory although no `try_` form names them.
const DRAWING: [&str; 39] = [
    "mem::Shared.make_mut",
    "mem::wire::Codec.decode",
    "mem::wire::Codec.decode_from",
    "mem::wire::Codec.encode_as",
    "mem::wire::Codec.schema",
    "text::json::Codec.decode",
    "text::construction::Codec.decode",
    "task::Scope.spawn",
    "task::Scope.spawn_sized",
    "task::race",
    "sys::Fs.read_all",
    "sys::Domain.run",
    "sys::Domain.run_sized",
    "sys::Abandoner.run",
    "sys::Abandoner.run_sized",
    "sys::Minter.of",
    "sys::Cx.send",
    "sys::Cx.after",
    "sys::Cx.after_ns",
    "sys::Cx.spawn",
    "sys::Stepper.start",
    "sys::Stepper.resume",
    "sys::Stepper.deliver",
    "sys::Stepper.step",
    "sys::sim::fs",
    "sys::sim::stdio",
    "hw::Dma.of",
    "hw::DmaShared.of",
    "hw::PerCpu.of",
    "hw::sim::Registers.of",
    "hw::sim::Registers.queue",
    "hw::sim::Registers.queue_at",
    "boot::Kernel.regions",
    "boot::Kernel.modules",
    "boot::Kernel.pci_functions",
    "boot::Kernel.anchor",
    "boot::Machine.enter",
    "host::Runner.run",
    "testing::world",
];

/// The supplied operations without a `try_` name that draw memory and fail, rather than trap,
/// when it is exhausted.
const FAILING_DRAWS: [&str; 8] = [
    "mem::wire::Codec.canonical",
    "boot::Services.read_file",
    "boot::Services.list_directory",
    "boot::Services.boot_device",
    "boot::Services.get_variable",
    "boot::Services.event_log",
    "boot::Services.submit_tpm",
    "boot::Services.memory_map",
];

/// The supplied operations whose owning chapters name traps other than exhausted memory: stale
/// handles, indexes past an end, duplicate windows and bindings, CPUs past the last, and
/// exhausted capability tables, cells and runtime records.
const TRAPPING: [&str; 32] = [
    "mem::Alloc.split",
    "mem::Arena.at",
    "mem::Pool.at",
    "mem::Pool.at_mut",
    "mem::Pool.remove",
    "text::Interner.resolve",
    "task::Join.join",
    "task::Executor.coarse",
    "task::Executor.split",
    "task::Executor.serial",
    "boot::Kernel.policy",
    "sys::revocable",
    "sys::Minter.mint",
    "sys::derive",
    "sys::Abandoner.run",
    "sys::Abandoner.run_sized",
    "sys::Fs.subdir",
    "sys::Fs.read_only",
    "sys::Net.only_host",
    "sys::Clock.coarse",
    "sys::WallClock.coarse",
    "sys::sim::net",
    "sys::sim::random",
    "hw::sim::Registers.mmio",
    "hw::sim::Registers.ports",
    "hw::sim::Registers.set_at",
    "hw::sim::Registers.last_written_at",
    "hw::IrqShared.install",
    "hw::IrqShared.install_on",
    "hw::IrqShared.install_msi",
    "hw::runtime_record",
    "hw::hold",
];

/// The supplied operations that change an atomic count without a type with interior mutability.
const COUNTED: [&str; 2] = ["mem::Shared.share", "mem::Shared.make_mut"];

/// The supplied operations calling a component's handlers on the calling task, whose digests
/// hold the declared digest of a call through `sys::Component`.
const HANDLING: [&str; 3] = [
    "sys::Stepper.start",
    "sys::Stepper.deliver",
    "sys::Stepper.step",
];

/// The std types storing their contents inline, which never draw memory.
const INLINE: [&str; 2] = ["text::Fixed", "mem::Bounded"];

/// The device, priv and sync facts the owning chapters state for pointers, register windows,
/// DMA and privileged intrinsics, by the canonical path of an operation or of the type it is a
/// member of.
pub fn stated(path: &str) -> Option<Digest> {
    let (owner, member) = path.rsplit_once('.').unwrap_or((path, ""));
    let mut digest = Digest::none();
    match owner {
        "hw::Ptr" => {
            digest.sync =
                matches!(member, "read" | "write" | "copy") || member.starts_with("atomic_")
        }
        "hw::fence" => digest.sync = true,
        "hw::UserAddr" if member == "copy_in" => digest.sync = true,
        "hw::Mmio" | "hw::Ports" | "hw::Dma" | "hw::DmaShared" | "hw::dma_fence" => {
            digest.device = true
        }
        "hw::Ring0" => digest.privileged = true,
        _ => return None,
    }
    Some(digest)
}

impl Checker<'_> {
    /// The digest of a supplied operation: waiting as its head states and terminating unless it
    /// waits; the facts its owning chapter states; drawing memory when it grows or makes
    /// storage; the sync fact on a type with interior mutability or an atomic count; the
    /// primitive capabilities it receives or is a member of, other than allocators; float use
    /// when its head takes or gives a float; the class of its operation under the target; and
    /// the declared digest of the handlers it calls.
    /// A head demanding `pure` or `noalloc` leaves out what it excludes.
    pub fn supplied(&self, item: ItemId) -> Digest {
        let Some(head) = self.fn_sig(item) else {
            return Digest::declared(false, &Default::default());
        };
        let path = self.db.canonical(item);
        let mut digest = stated(&path).unwrap_or_else(Digest::none);
        digest.wait = head.wait;
        digest.terminates = !head.wait;
        digest.float = head
            .params
            .iter()
            .map(|param| &param.ty)
            .chain([&head.result])
            .any(|ty| ty.scalar().is_some_and(super::ty::Scalar::is_float));
        if head.limits.pure {
            return digest;
        }
        digest.class = reversibility::supplied(&path, self.db.target);
        digest.alloc = !head.limits.noalloc && self.draws(item, &path, &head);
        let owner = match self.db.owners.get(&item) {
            Some(TypeKey::Item(owner)) => Some(*owner),
            _ => None,
        };
        digest.sync |= COUNTED.contains(&path.as_str())
            || owner.is_some_and(|owner| {
                super::props::INTERIOR.contains(&self.db.canonical(owner).as_str())
            });
        let capabilities: BTreeSet<ItemId> = head
            .params
            .iter()
            .filter_map(|param| match &param.ty {
                Ty::Item(capability, _) => Some(*capability),
                _ => None,
            })
            .chain(owner)
            .filter(|capability| {
                self.is_primitive_capability(*capability)
                    && self.db.canonical(*capability) != "mem::Alloc"
            })
            .collect();
        digest.capabilities = Some(capabilities);
        if HANDLING.contains(&path.as_str()) {
            digest.join(&Digest::declared(false, &Default::default()));
        }
        digest
    }

    /// Whether a supplied operation takes its read argument as the place holding it, whatever
    /// its type: `hw::Ptr.of`, whose pointer is to that place.
    pub fn reads_in_place(&self, item: ItemId) -> bool {
        self.db.is_std(item) && self.db.canonical(item) == "hw::Ptr.of"
    }

    /// Whether a supplied operation has a trap site: unless its head demands `notrap`, when it
    /// draws memory and neither is a `try_` form nor fails on exhaustion, or its owning chapter
    /// names a trap.
    pub fn supplied_traps(&self, item: ItemId) -> bool {
        let Some(head) = self.fn_sig(item) else {
            return true;
        };
        if head.limits.notrap {
            return false;
        }
        let path = self.db.canonical(item);
        let member = path
            .rsplit_once('.')
            .map_or(path.as_str(), |(_, member)| member);
        let exhausts = self.draws(item, &path, &head)
            && !member.starts_with("try_")
            && !FAILING_DRAWS.contains(&path.as_str());
        exhausts || TRAPPING.contains(&path.as_str())
    }

    /// Whether a supplied operation draws memory: a `try_` form, an operation with one, or one
    /// its owning chapter names as drawing, on a type that does not store its contents inline.
    fn draws(&self, item: ItemId, path: &str, head: &FnSig) -> bool {
        if head.limits.pure || head.limits.noalloc {
            return false;
        }
        let (owner, member) = path.rsplit_once('.').unwrap_or(("", path));
        if INLINE.contains(&owner) {
            return false;
        }
        if DRAWING.contains(&path) || FAILING_DRAWS.contains(&path) || member.starts_with("try_") {
            return true;
        }
        self.db
            .owners
            .get(&item)
            .is_some_and(|key| !self.db.members(*key, &format!("try_{member}")).is_empty())
    }
}
