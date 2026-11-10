//! Lowering: the translation of the NIR of a final closure to an object of its target through
//! Cranelift, with the support routines, compiler stubs, read-only data and line tables the image
//! needs. Every Cranelift call of the toolchain lies in this module and its submodules.
//!
//! Each function lowered is an instance: a NIR function, a std function the toolchain supplies,
//! a derived method or supplied conformance, the release of a type, or glue adapting one calling
//! form to another, each with its type arguments. Lowering starts from the program's entry and
//! lowers every instance the lowered code reaches.

mod abi;
mod body;
mod call;
mod conform;
mod data;
mod fx;
mod grown;
mod held;
mod jobs;

pub use jobs::{COMPONENT, JOB};
pub use lines::SYMBOL as LINES;
pub use runtime::space::{
    BOOT_AREA as SPACE_BOOT, START as SPACE_START, START_STACK as SPACE_STACK,
};
pub(crate) mod host;
mod layout;
mod levels;
mod lines;
pub mod load;
pub mod machine;
mod nested;
mod place;
mod release;
mod runtime;
mod slots;
pub mod stack;
mod supplied;
mod symbols;
pub mod test_image;
mod transfer;
mod trap;
mod types;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use cranelift_codegen::ir::{self, Signature, UserFuncName};
use cranelift_codegen::isa::{CallConv, OwnedTargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::{Context, ir::SourceLoc};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::check::Checker;
use crate::check::db::{ItemId, PackageId, TypeKey};
use crate::check::ty::{self as ty, Args, Ty};
use crate::nir::{Role, Span};
use crate::workspace::{Arch, Level, Target};
pub use abi::Abi;
use fx::Fx;
use layout::Layouts;
use runtime::Routine;
use stack::{Demand, Frame};

/// What a lowered function computes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Instance {
    /// A NIR function of an item's unit, with the arguments of the item's generic parameters.
    Nir {
        item: ItemId,
        role: Role,
        index: u32,
        args: Args,
    },
    /// A std function without a body, which the toolchain supplies.
    Supplied { item: ItemId, args: Args },
    /// A method a derive writes for its owner applied to `args`.
    Derived {
        owner: ItemId,
        method: &'static str,
        args: Args,
    },
    /// A method of an interface that a type implements through a conformance the toolchain
    /// supplies; `args` are the method's own type arguments.
    Conformance { ty: Ty, method: String, args: Args },
    /// An operation of a protocol, or its summary, called through an outbox.
    Operation { protocol: ItemId, method: u32 },
    /// An operation of a protocol called as a member of `sys::Wire`, encoding its call.
    Encoding { protocol: ItemId, method: u32 },
    /// The release of a value of a type.
    Release(Ty),
    /// The code of a function value naming a function: the instance called through the ABI of
    /// function values.
    Value(Box<Instance>),
    /// The code a `Dyn[I]` value calls for method `method` of `interface` on a value of `ty`.
    Dyn {
        ty: Ty,
        interface: ItemId,
        args: Args,
        method: u32,
    },
    /// A contract function computing the two sides of the comparison its result is, which the
    /// report of its failing check shows.
    Sides(Box<Instance>),
}

/// Code lowering writes for runtime use that no instance names, once per key: the rendering of
/// a template, and the code tasks run around their entries.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Glue {
    /// Renders a template over the record of its arguments' addresses.
    Template(Vec<supplied::render::Piece>, Vec<Ty>),
    /// Runs a task's entry, a function value of a type, and records how it ended.
    Entry(ty::FnTy),
    /// What a scope's child does once its entry ends: records its failure in the scope, the
    /// scope's failure type converting it when the spawn is lexical.
    ChildEnd(ty::FnTy, Option<Ty>),
    /// What the executor thread does once a scope's child has ended.
    ChildDone,
    /// What a race's task does once its entry ends: decides the race when first.
    RaceEnd,
    /// What the executor thread does once a race's task has ended.
    RaceDone,
    /// What a domain's first task does once its entry ends: closes the domain.
    DomainEnd,
    /// What the executor thread does once a domain's first task has ended.
    DomainDone,
    /// What the executor thread does once a kernel anchor's first task has ended.
    AnchorDone,
    /// A runner's component task, for its state, Env and message types.
    Component(Ty, Ty, Ty),
    /// What the executor thread does once a component task has ended.
    ComponentDone,
    /// What the executor thread does once a keyed task has ended.
    KeyedDone,
    /// The remainder of two floats of a width.
    FloatRemainder(supplied::floats::Width),
    /// The fused multiply-add of floats of a width.
    FusedMulAdd(supplied::floats::Width),
    /// Writes a value of a type in a codec's format.
    Encode(supplied::codecs::Format, Ty),
    /// Writes a value of a type, the second, through the calls of an encoder of the first.
    EncodeCalls(Ty, Ty),
    /// Reads a value of a type in a codec's format, or through the calls of a decoder.
    Decode(supplied::codecs::DecodeFormat, Ty),
    /// Makes a nested call from its record.
    NestedThunk,
    /// Writes the canonical encoding of a decoded value of a type to new bytes.
    Canonical(Ty),
    /// Reads a uvar of the wire format.
    WireUvar,
    /// Reads a JSON number's text.
    JsonNumber,
    /// Reads a JSON string.
    JsonString,
    /// Passes over a JSON value.
    JsonSkip,
    /// Reads a word of construction text.
    ConstructionWord,
    /// Reads a number of construction text.
    ConstructionNumber,
    /// Reads a float of construction text.
    ConstructionFloat,
    /// Reads a literal of construction text.
    ConstructionLiteral(supplied::codecs::construction::Literal),
    /// Passes over a value of construction text.
    ConstructionSkip,
    /// Decodes base64 text.
    Base64,
    /// Whether bytes are well-formed UTF-8.
    Utf8Valid,
    /// Whether a scalar value is a rejected code point.
    Rejected,
    /// Writes UTF-8 text to a renderer as a char or text literal of a quote.
    RenderLiteral(u8),
    /// Writes a value of a type in a trap report.
    Written(Ty),
    /// Withdraws a stopped sender's wait for room in a channel.
    WithdrawSender,
    /// Withdraws a stopped receiver's wait for a value in a channel.
    WithdrawReceiver,
    /// Withdraws a stopped task's requests of the cowns of an `await when`.
    WithdrawWhen,
    /// Settles a runner whose run a stopped task makes.
    WithdrawRun,
    /// Withdraws a stopped poster's entry from a runner's intake.
    WithdrawPost,
    /// Withdraws a stopped holder's wait on its stepper.
    WithdrawStepper,
    /// Withdraws a waiting operation from the sim waiter slot its record names.
    SimWithdraw,
    /// Withdraws a stopped task's wait on an `hw::Signal` of a type.
    WithdrawSignal(Ty),
    /// Compares two values of a type structurally, for `testing::same`.
    Same(Ty),
    /// Calls the interrupt fn of a device of a type behind an IRQ-safe lock, the interrupt fn
    /// being of a function type, with the device's record.
    IrqDevice(Ty, ty::FnTy),
    /// Moves a value of a type into another domain's reach.
    Transfer(Ty),
    /// Releases a value of a type of the release group whose first member is the type.
    Release(Ty),
    /// Moves the captures of a task closure, the closure function's instance, into another
    /// domain's reach.
    Captures(Instance),
    /// Calls a hook of a kernel policy of a type.
    Policy(machine::schedule::Hook, Ty),
    /// Calls, for a function type, the target its gate record names.
    Gate(ty::FnTy),
    /// A grown call's wrapper, taking the call's arguments, a function value's code first.
    Grown(grown::Called),
    /// A grown call's thunk, making the call from the record of its arguments.
    GrownThunk(grown::Called),
}

impl Glue {
    /// The family of glues support routines call through addresses that the glue is of.
    fn family(&self) -> Option<stack::Family> {
        match self {
            Glue::Entry(_) => Some(stack::Family::Task),
            Glue::ChildEnd(..) | Glue::RaceEnd | Glue::DomainEnd => Some(stack::Family::End),
            Glue::ChildDone
            | Glue::RaceDone
            | Glue::DomainDone
            | Glue::AnchorDone
            | Glue::ComponentDone
            | Glue::KeyedDone => Some(stack::Family::Done),
            Glue::WithdrawSender
            | Glue::WithdrawReceiver
            | Glue::WithdrawWhen
            | Glue::WithdrawRun
            | Glue::WithdrawPost
            | Glue::WithdrawStepper
            | Glue::SimWithdraw
            | Glue::WithdrawSignal(_) => Some(stack::Family::Withdraw),
            Glue::Written(_) => Some(stack::Family::Written),
            Glue::Captures(_) => Some(stack::Family::Captures),
            Glue::Policy(..) => Some(stack::Family::Policy),
            _ => None,
        }
    }

    /// Whether a stopping task does not stop inside the glue: one completing the end of a
    /// task or of a construct.
    fn critical(&self) -> bool {
        matches!(
            self,
            Glue::ChildEnd(..)
                | Glue::ChildDone
                | Glue::RaceEnd
                | Glue::RaceDone
                | Glue::DomainEnd
                | Glue::DomainDone
                | Glue::AnchorDone
                | Glue::ComponentDone
                | Glue::KeyedDone
                | Glue::WithdrawSender
                | Glue::WithdrawReceiver
                | Glue::WithdrawWhen
                | Glue::WithdrawRun
                | Glue::WithdrawPost
                | Glue::WithdrawStepper
                | Glue::SimWithdraw
                | Glue::WithdrawSignal(_)
                | Glue::Gate(_)
        )
    }
}

/// The std types whose operations a stopping task completes before it stops, beside those
/// with interior mutability.
const CRITICAL_OWNERS: [&str; 17] = [
    "sys::Domain",
    "task::Sender",
    "task::Receiver",
    "task::Scope",
    "sys::Cx",
    "sys::Outbox",
    "mem::Shared",
    "task::Cown",
    "hw::Ring0",
    "hw::Mmio",
    "hw::Dma",
    "hw::DmaShared",
    "hw::Iommu",
    "hw::IoDomain",
    "hw::PciFunction",
    "hw::PciKeeper",
    "hw::PciPort",
];

/// The outcome of lowering a final closure.
pub struct Lowered {
    pub object: Vec<u8>,
    /// The frame and call sites of each function lowered, and the `stack N` demands on them,
    /// for the stack limits check.
    pub frames: Vec<Frame>,
    pub demands: Vec<Demand>,
    /// The task entries lowered, for the measurements of the freeze.
    pub tasks: Vec<stack::Task>,
    /// The two entities of each pair lowered with one host symbol name, as diagnostics name them.
    pub collisions: Vec<(String, String)>,
    /// The entries a bundle adds to loaded registries.
    pub contributed: Vec<load::Contributed>,
    /// For a nanos program's bundle, the stack its job's task needs, and the name of each
    /// component entry with the stack its task needs.
    pub job: Option<u64>,
    pub components: Vec<(String, u64)>,
}

/// What a final build lowers: the program's entry function and its target's entry landing fn,
/// for the final-link roots whose contributions its registries admit.
pub struct Program {
    pub entry: ItemId,
    pub landing: ItemId,
    pub roots: Vec<PackageId>,
    /// The package names of the final closure with the prefix their symbols carry.
    pub prefixes: HashMap<String, String>,
    /// The content of the image's `.nan` section.
    pub section: Vec<u8>,
    /// The codegen level of the program's manifest for the target's architecture.
    pub level: Option<Level>,
    /// A kernel's interface, when it exports the load ABI, and the loaded registries of its
    /// final closure.
    pub interface: Option<Rc<load::Interface>>,
    pub loaded: Vec<ItemId>,
    /// For a kernel running nanos jobs in isolated spaces, the image of their user-mode runtime.
    pub runtime: Option<Vec<u8>>,
}

/// What a bundle's build lowers: the entries its contributions add to loaded registries, or a
/// nanos program's job entry running its entry function or its component entries, by name.
pub struct Bundle {
    pub contributions: Vec<(ItemId, ItemId)>,
    pub entry: Option<ItemId>,
    pub components: Vec<(String, ItemId)>,
    pub roots: Vec<PackageId>,
    pub prefixes: HashMap<String, String>,
    /// The codegen level of the kernel's interface.
    pub level: Option<Level>,
}

pub struct Lowering<'c, 'a> {
    pub ck: &'c Checker<'a>,
    pub module: ObjectModule,
    pub linking: load::Linking,
    pub layouts: Layouts,
    isa: OwnedTargetIsa,
    /// The codegen level the image's code is generated for, none for the base set.
    level: Option<Level>,
    prefixes: HashMap<String, String>,
    roots: Vec<PackageId>,
    instances: HashMap<Instance, FuncId>,
    /// The instance each host symbol name names.
    named: HashMap<String, Instance>,
    /// The distinct instances lowered with one host symbol name.
    collisions: Vec<(Instance, Instance)>,
    routines: HashMap<runtime::Routine, FuncId>,
    /// The functions that keep no address given to them once they return: the instances, the
    /// glue and the routines over byte ranges.
    keeping_none: HashSet<FuncId>,
    /// The instances and routines declared and not yet defined.
    queue: Vec<Pending>,
    releases: HashMap<Ty, bool>,
    /// The release group of each type asked for, once known.
    release_groups: HashMap<Ty, Option<Rc<Vec<Ty>>>>,
    transfers: HashMap<Ty, bool>,
    glues: HashMap<Glue, FuncId>,
    /// The number of glue functions and tables named so far.
    glue: u32,
    state_id: Option<DataId>,
    thread_id: Option<DataId>,
    /// The image's compiler stubs, and a machine image's vector seam, the data objects it reserves
    /// by name, the sizes of its interrupt stacks, and the stacks of nonzero `ist` with how many
    /// each CPU has, once declared.
    machine_stubs: HashMap<machine::Stub, FuncId>,
    vector_seam: Option<FuncId>,
    /// A kernel's preemption glue, which its vector seam calls on an interrupted task's stack.
    preemption: Option<FuncId>,
    reserved: HashMap<&'static str, DataId>,
    irq_stacks: Option<DataId>,
    ist_stacks: Option<(DataId, u64)>,
    /// The image's routine reserve, once a routine reads it.
    routine_reserve: Option<DataId>,
    /// The walks moving the values of the cowns `await when`s hold.
    pub held_walks: HashSet<FuncId>,
    pub data: data::Data,
    pub lines: lines::Lines,
    /// The task entries whose own `limits stack N` passes the default task stack, by the code a
    /// function value naming them holds.
    pub entry_stacks: Vec<(FuncId, u64)>,
    frames: Vec<Frame>,
    pub demands: Vec<Demand>,
    pub tasks: Vec<stack::Task>,
    /// For a test image, what its tests' runs need, and the sim table once declared.
    test: Option<test_image::Runtime>,
    sims: Option<DataId>,
    /// For a bundle, the entries it adds to loaded registries.
    contributed: Vec<load::Contributed>,
    /// For an image whose task stacks grow, its grown calls.
    growth: Option<grown::Growth>,
    /// The nested calls it makes.
    nested: nested::Nested,
    /// For a kernel running nanos jobs in isolated spaces, the image of their user-mode runtime.
    pub runtime_image: Option<Vec<u8>>,
}

/// A function declared and not yet defined.
enum Pending {
    Instance(Instance, FuncId),
    Routine(runtime::Routine, FuncId),
}

/// The settings an image's code is generated with: the base set of its architecture, the x86-64
/// one with SSE2 or the A64 one of Armv8.0-A, with the extensions of its codegen level, frame
/// pointers in every function, Cranelift's stack switching where it has it, and the canonical NaN
/// as the NaN every float operation returns. A host image is position-independent and links
/// against glibc with inline stack probes for its guard pages; a machine image loads at a fixed
/// address and is not position-independent, so its code reaches its own functions and data
/// directly, with no global offset table and no stack probes.
fn isa(target: Target, level: Option<Level>) -> OwnedTargetIsa {
    let mut flags = settings::builder();
    let host = target.is_host();
    let triple = match (target.arch(), host) {
        (Some(Arch::Aarch64), true) => "aarch64-unknown-linux-gnu",
        (Some(Arch::Aarch64), false) => "aarch64-unknown-none-elf",
        (_, true) => "x86_64-unknown-linux-gnu",
        (_, false) => "x86_64-unknown-none-elf",
    };
    let on_host = if host { "true" } else { "false" };
    for (name, value) in [
        ("opt_level", "speed"),
        ("is_pic", on_host),
        ("preserve_frame_pointers", "true"),
        ("stack_switch_model", "basic"),
        ("enable_probestack", on_host),
        ("probestack_strategy", "inline"),
        ("unwind_info", "false"),
        ("tls_model", "elf_gd"),
        ("enable_nan_canonicalization", "true"),
    ] {
        flags.set(name, value).expect("a Cranelift setting");
    }
    let mut builder = cranelift_codegen::isa::lookup_by_name(triple).expect("the target's backend");
    for extension in level.map_or(&[][..], Level::extensions) {
        builder.enable(extension).expect("a Cranelift ISA setting");
    }
    builder
        .finish(settings::Flags::new(flags))
        .expect("the target's settings")
}

/// Lowers the reach of a program's final build.
pub fn lower(ck: &Checker, program: &Program) -> Lowered {
    let linking = load::Linking::Image;
    let mut lowering = Lowering::new(
        ck,
        program.prefixes.clone(),
        program.roots.clone(),
        program.level,
        None,
        linking,
    );
    host::entry_seam(&mut lowering, program);
    lowering.drain();
    lowering.finish(Some(&program.section))
}

/// Lowers the reach of a final build for a machine target: the entry function and the target's
/// entry landing fn behind the machine entry seam, into an object the image writer turns into an
/// image.
pub fn lower_machine(ck: &Checker, program: &Program) -> Lowered {
    let linking = match &program.interface {
        Some(interface) => load::Linking::Kernel(interface.clone()),
        None => load::Linking::Image,
    };
    let mut lowering = Lowering::new(
        ck,
        program.prefixes.clone(),
        program.roots.clone(),
        program.level,
        None,
        linking,
    );
    lowering.runtime_image = program.runtime.clone();
    let boot = lowering.boot_glue(program);
    let stack = lowering.boot_stack(program);
    lowering.entry_seam(boot, stack);
    lowering.export_abi(&program.loaded);
    lowering.drain();
    lowering.finish(None)
}

/// Lowers the user-mode runtime of a kernel's isolated spaces: its start routine, its runtime
/// state and every support routine a kernel exports, into an object the bundle writer links.
pub fn lower_runtime(ck: &Checker, program: &Program) -> Lowered {
    let mut lowering = Lowering::new(
        ck,
        program.prefixes.clone(),
        program.roots.clone(),
        program.level,
        None,
        load::Linking::Runtime,
    );
    lowering.state_data();
    for routine in load::ROUTINES {
        lowering.routine(routine);
    }
    lowering.space_start();
    lowering.drain();
    lowering.finish(None)
}

/// Lowers a bundle: the entries its contributions add to loaded registries, its job entry or its
/// component entries, with every function they reach, importing the load ABI.
pub fn lower_bundle(ck: &Checker, bundle: &Bundle) -> Lowered {
    let linking = load::Linking::Bundle;
    let mut lowering = Lowering::new(
        ck,
        bundle.prefixes.clone(),
        bundle.roots.clone(),
        bundle.level,
        None,
        linking,
    );
    for (contribution, registry) in &bundle.contributions {
        let contributed = lowering.contribute(*contribution, *registry);
        lowering.contributed.push(contributed);
    }
    let job = bundle.entry.map(|entry| lowering.job_entry(entry));
    let components: Vec<(String, FuncId)> = bundle
        .components
        .iter()
        .map(|(name, state)| (name.clone(), lowering.component_entry(name, *state)))
        .collect();
    lowering.drain();
    let lowered = lowering.finish(None);
    let job = job
        .map(|(entry, stack)| stack.unwrap_or_else(|| stack::grown_start(&lowered.frames, entry)));
    let components = components
        .into_iter()
        .map(|(name, entry)| (name, stack::grown_start(&lowered.frames, entry)))
        .collect();
    Lowered {
        job,
        components,
        ..lowered
    }
}

impl<'c, 'a> Lowering<'c, 'a> {
    /// A lowering into a new object, for a codegen level when `level` is given, of a test image
    /// when `test` is given.
    fn new(
        ck: &'c Checker<'a>,
        prefixes: HashMap<String, String>,
        roots: Vec<PackageId>,
        level: Option<Level>,
        test: Option<test_image::Runtime>,
        linking: load::Linking,
    ) -> Self {
        let isa = isa(ck.db.target, level);
        let builder = ObjectBuilder::new(
            isa.clone(),
            "nan",
            cranelift_module::default_libcall_names(),
        )
        .expect("an object builder");
        Lowering {
            ck,
            module: ObjectModule::new(builder),
            linking,
            layouts: Layouts::new(),
            isa,
            level,
            prefixes,
            roots,
            instances: HashMap::new(),
            named: HashMap::new(),
            collisions: Vec::new(),
            routines: HashMap::new(),
            keeping_none: HashSet::new(),
            queue: Vec::new(),
            releases: HashMap::new(),
            release_groups: HashMap::new(),
            transfers: HashMap::new(),
            glues: HashMap::new(),
            glue: 0,
            state_id: None,
            thread_id: None,
            machine_stubs: HashMap::new(),
            vector_seam: None,
            preemption: None,
            reserved: HashMap::new(),
            irq_stacks: None,
            routine_reserve: None,
            held_walks: HashSet::new(),
            ist_stacks: None,
            data: data::Data::default(),
            lines: lines::Lines::default(),
            entry_stacks: Vec::new(),
            frames: Vec::new(),
            demands: Vec::new(),
            tasks: Vec::new(),
            test,
            sims: None,
            contributed: Vec::new(),
            growth: ck.db.target.is_nanos().then(grown::Growth::default),
            nested: nested::Nested::default(),
            runtime_image: None,
        }
    }

    /// Whether the image's code runs on a machine's runtime: an image of a UEFI or bare target, or
    /// a bundle of a nanos target, which runs on its kernel's.
    pub fn machine(&self) -> bool {
        self.ck.db.target.is_machine() || self.ck.db.target.is_nanos()
    }

    /// Whether the image's code keeps floats and lanes in general registers as bit patterns, as
    /// code of a machine target does.
    pub fn float_bits(&self) -> bool {
        self.ck.db.target.is_machine()
    }

    /// Whether the image runs a kernel's executor, under interrupts: a machine image other than
    /// a test image, whose runs keep interrupts disabled.
    pub fn kernel(&self) -> bool {
        self.machine() && !self.seeded()
    }

    /// Whether the image's executors are a kernel's CPUs, under interrupts: a kernel's, unlike
    /// the one executor of an isolated space's runtime, which runs in user mode and serves
    /// capabilities as its kernel does.
    pub fn on_cpus(&self) -> bool {
        self.kernel() && !self.user()
    }

    /// The function lowering an instance, declared now and lowered later when new.
    pub fn instance(&mut self, instance: Instance) -> FuncId {
        if let Some(id) = self.instances.get(&instance) {
            return *id;
        }
        let abi = self.abi(&instance);
        let mut name = symbols::name(self, &instance);
        match self.named.get(&name) {
            Some(named) => {
                self.collisions.push((named.clone(), instance.clone()));
                self.glue += 1;
                name = format!("_NXcollision_{}", self.glue);
            }
            None => {
                self.named.insert(name.clone(), instance.clone());
            }
        }
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &abi.signature())
            .unwrap_or_else(|error| panic!("declaring {name}: {error}"));
        self.instances.insert(instance.clone(), id);
        self.keeping_none.insert(id);
        self.queue.push(Pending::Instance(instance, id));
        id
    }

    /// Defines every function declared until none is left.
    fn drain(&mut self) {
        while let Some(pending) = self.queue.pop() {
            match pending {
                Pending::Instance(instance, id) => {
                    let abi = self.abi(&instance);
                    let flags = self.frame_flags(&instance);
                    let ran = self.target_slot(&instance);
                    self.define(id, abi.signature(), flags, |lo, fx| {
                        if let Some(slot) = ran {
                            let data = lo.ran_data();
                            fx.mark = Some((lo.data_ref(fx, data), slot));
                        }
                        lo.lower_instance(fx, &instance, &abi)
                    });
                    self.describe_frame(&instance, &abi, id);
                }
                Pending::Routine(routine, id) => {
                    let flags = match routine {
                        Routine::TaskStart
                        | Routine::Grow
                        | Routine::GrownStart
                        | Routine::Nested => lines::HIDDEN,
                        _ => lines::HIDDEN | lines::CRITICAL,
                    };
                    self.define(id, routine.signature(), flags, |lo, fx| {
                        runtime::define(lo, fx, routine)
                    });
                }
            }
        }
    }

    fn lower_instance(&mut self, fx: &mut Fx, instance: &Instance, abi: &Abi) {
        match instance {
            Instance::Nir {
                item,
                role: Role::Body,
                args,
                ..
            } if !self.float_bits()
                && self.ck.db.is_std(*item)
                && supplied::simd::vectorized(&self.ck.db.canonical(*item)) =>
            {
                supplied::simd::lower_vector(self, fx, abi, *item, args)
            }
            Instance::Nir {
                item,
                role,
                index,
                args,
            } => body::lower(self, fx, abi, (*item, *role, *index), args, false),
            Instance::Supplied { item, args } => supplied::lower(self, fx, abi, *item, args),
            Instance::Derived {
                owner,
                method,
                args,
            } => conform::derived(self, fx, abi, *owner, method, args),
            Instance::Conformance { ty, method, .. } => {
                conform::supplied(self, fx, abi, ty, method)
            }
            Instance::Operation { protocol, method } => {
                supplied::tasks::protocols::call(self, fx, abi, *protocol, *method as usize)
            }
            Instance::Encoding { protocol, method } => {
                supplied::codecs::protocols::encode_call(self, fx, abi, *protocol, *method as usize)
            }
            Instance::Release(ty) => release::define(self, fx, ty),
            Instance::Value(inner) => call::value_glue(self, fx, abi, inner),
            Instance::Dyn {
                ty,
                interface,
                args,
                method,
            } => call::dyn_glue(self, fx, abi, ty, *interface, args, *method),
            Instance::Sides(inner) => {
                let Instance::Nir {
                    item,
                    role,
                    index,
                    args,
                } = &**inner
                else {
                    panic!("the sides of {inner:?}");
                };
                body::lower(self, fx, abi, (*item, *role, *index), args, true);
            }
        }
    }

    /// Names the frame of an instance just defined and the `stack N` its function-typed and
    /// `Dyn` parameters' types demand, and records the demands of its own limits and of the
    /// interface method it implements.
    fn describe_frame(&mut self, instance: &Instance, abi: &Abi, id: FuncId) {
        let frame = self.frames.last_mut().expect("the frame just defined");
        frame.params = abi
            .params
            .iter()
            .enumerate()
            .filter_map(|(position, (nty, _))| match nty.ty() {
                Ty::Fn(fn_ty) if fn_ty.kind == crate::syntax::ast::FnKind::Task => {
                    Some((position, None))
                }
                Ty::Fn(fn_ty) => Some((position, fn_ty.limits.stack)),
                Ty::Dyn(_) => Some((position, None)),
                _ => None,
            })
            .collect();
        let item = match instance {
            Instance::Nir { item, .. } | Instance::Supplied { item, .. } => *item,
            Instance::Release(released @ Ty::Item(item, _)) => {
                frame.name = self.ck.show(released, self.ck.db.module_of(*item));
                return;
            }
            _ => return,
        };
        frame.name = self.ck.db.canonical(item);
        let Instance::Nir { role, index, .. } = instance else {
            return;
        };
        let unit = self.ck.unit(item).expect("a unit lowered");
        let function = unit
            .function(*role, *index)
            .expect("a function of the unit");
        let mut demanded = Vec::new();
        let head = match role {
            Role::Closure => {
                demanded.extend(function.closure.as_ref().and_then(|head| head.limits.stack));
                function.span
            }
            Role::Body => {
                demanded.extend(self.ck.fn_sig(item).and_then(|sig| sig.limits.stack));
                if let Some(implemented) = self.ck.implemented_method(item)
                    && let crate::check::sig::SigKind::Interface { methods }
                    | crate::check::sig::SigKind::Protocol {
                        operations: methods,
                        ..
                    } = &self.ck.sig(implemented.interface).kind
                {
                    demanded.extend(methods[implemented.method].sig.limits.stack);
                }
                Span::at(self.ck.db.item(item).ast.token)
            }
            _ => return,
        };
        for stack in demanded {
            self.demands.push(Demand {
                judged: stack::Judged::Function(id),
                stack,
                report: stack::Report::Own {
                    head: stack::At { item, span: head },
                },
            });
        }
    }

    /// The line table flags of an instance's function: trap reports leave out the frames of
    /// std items and of the code lowering writes itself, and a stopping task does not stop
    /// inside a std function of a type with interior mutability or of `task::Sender`,
    /// `task::Receiver`, `task::Scope`, `sys::Cx`, `sys::Outbox`, `mem::Shared`, `task::Cown`
    /// or `hw::Ring0`.
    fn frame_flags(&self, instance: &Instance) -> u32 {
        let (hidden, critical) = match instance {
            Instance::Nir { item, args, .. } | Instance::Supplied { item, args } => {
                let std = self.ck.db.is_std(*item);
                (std, std && self.critical_owner(*item, args))
            }
            Instance::Operation { .. } => (true, true),
            _ => (true, false),
        };
        let hidden = if hidden { lines::HIDDEN } else { 0 };
        let critical = if critical { lines::CRITICAL } else { 0 };
        hidden | critical
    }

    /// Whether a std function's owner is a type whose operations a stopping task completes.
    fn critical_owner(&self, item: ItemId, args: &Args) -> bool {
        let Some(TypeKey::Item(owner)) = self.ck.db.owners.get(&item) else {
            return false;
        };
        let path = self.ck.db.canonical(*owner);
        let generics = self.ck.sig(*owner).generics.len();
        let ty = Ty::item(*owner, args.iter().take(generics).cloned().collect());
        CRITICAL_OWNERS.contains(&path.as_str()) || self.ck.has_interior(&ty)
    }

    /// Builds and compiles a function's body, with its line table flags.
    pub fn define(
        &mut self,
        id: FuncId,
        signature: Signature,
        flags: u32,
        build: impl FnOnce(&mut Self, &mut Fx),
    ) {
        let mut context = Context::new();
        context.func.signature = signature;
        context.func.name = UserFuncName::user(0, id.as_u32());
        let mut builder_context = FunctionBuilderContext::new();
        self.enter_definition(id);
        let recorded = {
            let mut fx = Fx::new(
                FunctionBuilder::new(&mut context.func, &mut builder_context),
                self.ck.db.target.arch() == Some(Arch::Amd64),
            );
            build(self, &mut fx);
            fx.b.seal_all_blocks();
            let recorded = std::mem::take(&mut fx.sites);
            fx.b.finalize(self.isa.frontend_config());
            recorded
        };
        self.leave_definition();
        let copy = self.routines.get(&runtime::Routine::Copy).copied();
        let keeping_none = &self.keeping_none;
        slots::share(&mut context.func, &|index| {
            let id = FuncId::from_u32(index);
            if Some(id) == copy {
                slots::Callee::Copying
            } else if keeping_none.contains(&id) {
                slots::Callee::Plain
            } else {
                slots::Callee::Keeping
            }
        });
        DUMP.with(|dump| {
            if let Some(texts) = dump.borrow_mut().as_mut() {
                let declaration = self.module.declarations().get_function_decl(id);
                let name = declaration.name.clone().unwrap_or_default();
                texts.push((name, context.func.display().to_string()));
            }
        });
        let sites = self.sites(&context.func, recorded);
        if let Err(error) = self.module.define_function(id, &mut context) {
            panic!(
                "compiling {}: {error:?}\n{}",
                self.module
                    .declarations()
                    .get_function_decl(id)
                    .name
                    .clone()
                    .unwrap_or_default(),
                context.func.display()
            );
        }
        let compiled = context.compiled_code().expect("a compiled function");
        let frame = compiled
            .buffer
            .frame_layout()
            .map_or(0, |layout| u64::from(layout.frame_to_fp_offset))
            + 16;
        let locations = compiled
            .buffer
            .get_srclocs_sorted()
            .iter()
            .filter(|location| !location.loc.is_default())
            .map(|location| (location.start, location.loc.bits()))
            .collect();
        self.lines
            .function(id, flags, compiled.buffer.total_size(), locations);
        self.frames.push(Frame {
            function: id,
            name: String::new(),
            size: frame,
            params: Vec::new(),
            sites,
        });
    }

    /// The call sites of a function built: those lowering classified as it emitted them, a
    /// direct call of an import bounded by the stub call bound, of a routine the image defines a
    /// routine call, of a grown call's wrapper grown, of another lowered function direct, and an
    /// unclassified call through an address unbounded.
    fn sites(
        &self,
        func: &ir::Function,
        recorded: Vec<(ir::Inst, stack::Site)>,
    ) -> Vec<stack::Site> {
        let mut recorded: HashMap<ir::Inst, stack::Site> = recorded.into_iter().collect();
        let mut sites = Vec::new();
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                let wrapper = match func.dfg.insts[inst] {
                    ir::InstructionData::Call { func_ref, .. } => {
                        match &func.dfg.ext_funcs[func_ref].name {
                            ir::ExternalName::User(name) => Some(FuncId::from_u32(
                                func.params.user_named_funcs()[*name].index,
                            )),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let site = recorded.remove(&inst);
                let grown = self
                    .growth
                    .as_ref()
                    .zip(wrapper)
                    .and_then(|(growth, callee)| {
                        growth.site(callee, site.as_ref().map(|site| site.kind.clone()))
                    });
                if let Some(kind) = grown {
                    let at = site.and_then(|site| site.at);
                    sites.push(stack::Site { kind, at });
                    continue;
                }
                if let Some(site) = site {
                    sites.push(site);
                    continue;
                }
                let kind = match func.dfg.insts[inst] {
                    ir::InstructionData::Call { func_ref, .. } => {
                        let ir::ExternalName::User(name) = &func.dfg.ext_funcs[func_ref].name
                        else {
                            continue;
                        };
                        let callee = FuncId::from_u32(func.params.user_named_funcs()[*name].index);
                        let declaration = self.module.declarations().get_function_decl(callee);
                        if declaration.linkage == Linkage::Import {
                            stack::Kind::Stub
                        } else if self.routines.values().any(|routine| *routine == callee) {
                            stack::Kind::Routine(callee)
                        } else {
                            stack::Kind::Direct {
                                callee,
                                bindings: Vec::new(),
                            }
                        }
                    }
                    ir::InstructionData::CallIndirect { .. } => stack::Kind::Bounded(None),
                    _ => continue,
                };
                sites.push(stack::Site { kind, at: None });
            }
        }
        sites
    }

    /// The glues and releases support routines call through addresses, by family.
    fn families(&self) -> HashMap<stack::Family, Vec<FuncId>> {
        let mut families: HashMap<stack::Family, Vec<FuncId>> = HashMap::new();
        for (glue, id) in &self.glues {
            if let Some(family) = glue.family() {
                families.entry(family).or_default().push(*id);
            }
        }
        families
            .entry(stack::Family::Held)
            .or_default()
            .extend(self.held_walks.iter().copied());
        for (instance, id) in &self.instances {
            if let Instance::Release(_) = instance {
                families
                    .entry(stack::Family::Release)
                    .or_default()
                    .push(*id);
            }
        }
        families
    }

    /// The function of a glue already asked for.
    pub fn glue(&self, glue: &Glue) -> Option<FuncId> {
        self.glues.get(glue).copied()
    }

    /// The function of a glue, declared and defined when first asked for.
    pub fn glue_function(
        &mut self,
        glue: Glue,
        signature: Signature,
        build: impl FnOnce(&mut Self, &mut Fx),
    ) -> FuncId {
        if let Some(id) = self.glues.get(&glue) {
            return *id;
        }
        self.glue += 1;
        let name = format!("_NXglue_{}", self.glue);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &signature)
            .expect("a glue function");
        let flags = match glue.critical() {
            true => lines::HIDDEN | lines::CRITICAL,
            false => lines::HIDDEN,
        };
        let walked = match &glue {
            Glue::Transfer(Ty::Item(item, args)) => {
                let ty = Ty::Item(*item, args.clone());
                Some(self.ck.show(&ty, self.ck.db.module_of(*item)))
            }
            _ => None,
        };
        self.glues.insert(glue, id);
        self.keeping_none.insert(id);
        self.define(id, signature, flags, build);
        if let Some(walked) = walked {
            self.frames.last_mut().expect("the frame just defined").name = walked;
        }
        id
    }

    /// Marks the code `fx` emits next as coming from a span of an item's file.
    pub fn locate(&mut self, fx: &mut Fx, item: ItemId, span: Span) {
        let location = self.lines.location(self.ck, item, span);
        fx.b.set_srcloc(SourceLoc::new(location));
    }

    /// A reference to a function from the function being built.
    pub fn func_ref(&mut self, fx: &mut Fx, id: FuncId) -> ir::FuncRef {
        self.module.declare_func_in_func(id, fx.b.func)
    }

    /// A reference to data from the function being built.
    pub fn data_ref(&mut self, fx: &mut Fx, id: DataId) -> ir::GlobalValue {
        self.module.declare_data_in_func(id, fx.b.func)
    }

    /// The layout of a type.
    pub fn layout(&mut self, ty: &Ty) -> Rc<layout::Layout> {
        self.layouts.of(self.ck, ty)
    }

    /// The object of the lowering, holding a `.nan` section with `section` when given.
    fn finish(mut self, section: Option<&[u8]>) -> Lowered {
        let entry_stacks = runtime::tasks::emit_entry_stacks(&mut self);
        let glues = self.finish_reserve();
        lines::emit(&mut self, entry_stacks, glues);
        self.finish_interrupts();
        self.emit_nested_needs();
        self.emit_needs();
        let frames = std::mem::take(&mut self.frames);
        let demands = std::mem::take(&mut self.demands);
        let tasks = std::mem::take(&mut self.tasks);
        let contributed = std::mem::take(&mut self.contributed);
        let collisions = std::mem::take(&mut self.collisions)
            .iter()
            .map(|(first, second)| {
                (
                    symbols::entity(self.ck, first),
                    symbols::entity(self.ck, second),
                )
            })
            .collect();
        let mut product = self.module.finish();
        if let Some(section) = section {
            let id = product.object.add_section(
                Vec::new(),
                b".nan".to_vec(),
                cranelift_object::object::SectionKind::Other,
            );
            product.object.append_section_data(id, section, 1);
        }
        Lowered {
            object: product.emit().expect("an object file"),
            frames,
            demands,
            tasks,
            collisions,
            contributed,
            job: None,
            components: Vec::new(),
        }
    }
}

/// The calling convention of every lowered function.
pub const CALL_CONV: CallConv = CallConv::SystemV;

thread_local! {
    /// While `dumping` runs, the symbol name and text of each function defined, in order.
    static DUMP: RefCell<Option<Vec<(String, String)>>> = const { RefCell::new(None) };
}

/// Runs `lower`, giving its result with the symbol name of each function it defines and that
/// function's text as Cranelift writes it before compiling it, in order of definition.
pub fn dumping<T>(lower: impl FnOnce() -> T) -> (T, Vec<(String, String)>) {
    DUMP.with(|dump| *dump.borrow_mut() = Some(Vec::new()));
    let result = lower();
    let texts = DUMP
        .with(|dump| dump.borrow_mut().take())
        .unwrap_or_default();
    (result, texts)
}

#[cfg(test)]
mod tests;
