//! Lowering: the translation of the NIR of a final closure to an `x86_64-host` object through
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
mod held;
pub(crate) mod host;
mod layout;
mod lines;
mod place;
mod release;
mod runtime;
pub mod stack;
mod supplied;
mod symbols;
pub mod test_image;
mod transfer;
mod trap;
mod types;

use std::collections::HashMap;
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
    /// What the executor thread does once a domain's first task has ended.
    DomainDone,
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
    /// Reads a value of a type in a codec's format.
    Decode(supplied::codecs::Format, Ty),
    /// Reads a uvar of the wire format.
    WireUvar,
    /// Reads a JSON number's text.
    JsonNumber,
    /// Reads a JSON string.
    JsonString,
    /// Passes over a JSON value.
    JsonSkip,
    /// Decodes base64 text.
    Base64,
    /// Whether bytes are well-formed UTF-8.
    Utf8Valid,
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
    /// Withdraws a waiting operation from the sim waiter slot its record names.
    SimWithdraw,
    /// Compares two values of a type structurally, for `testing::same`.
    Same(Ty),
    /// Moves a value of a type into another domain's reach.
    Transfer(Ty),
    /// Moves the captures of a task closure, the closure function's instance, into another
    /// domain's reach.
    Captures(Instance),
}

impl Glue {
    /// Whether a stopping task does not stop inside the glue: one completing the end of a
    /// task or of a construct.
    fn critical(&self) -> bool {
        matches!(
            self,
            Glue::ChildEnd(..)
                | Glue::ChildDone
                | Glue::RaceEnd
                | Glue::RaceDone
                | Glue::DomainDone
                | Glue::ComponentDone
                | Glue::KeyedDone
                | Glue::WithdrawSender
                | Glue::WithdrawReceiver
                | Glue::WithdrawWhen
                | Glue::WithdrawRun
                | Glue::WithdrawPost
                | Glue::SimWithdraw
        )
    }
}

/// The std types whose operations a stopping task completes before it stops, beside those
/// with interior mutability.
const CRITICAL_OWNERS: [&str; 8] = [
    "sys::Domain",
    "task::Sender",
    "task::Receiver",
    "task::Scope",
    "sys::Cx",
    "sys::Outbox",
    "mem::Shared",
    "task::Cown",
];

/// The outcome of lowering a final closure.
pub struct Lowered {
    pub object: Vec<u8>,
    /// The frame and call sites of each function lowered, and the `stack N` demands on them,
    /// for the stack limits check.
    pub frames: Vec<Frame>,
    pub demands: Vec<Demand>,
}

/// What a final build lowers: the program's entry function and its target's entry landing fn,
/// for the final-link roots whose contributions its registries admit.
pub struct Program {
    pub entry: ItemId,
    pub landing: ItemId,
    pub roots: Vec<PackageId>,
    /// The package names of the final closure with the prefix their symbols carry.
    pub prefixes: HashMap<String, String>,
}

pub struct Lowering<'c, 'a> {
    pub ck: &'c Checker<'a>,
    pub module: ObjectModule,
    pub layouts: Layouts,
    isa: OwnedTargetIsa,
    prefixes: HashMap<String, String>,
    roots: Vec<PackageId>,
    instances: HashMap<Instance, FuncId>,
    routines: HashMap<runtime::Routine, FuncId>,
    /// The instances and routines declared and not yet defined.
    queue: Vec<Pending>,
    releases: HashMap<Ty, bool>,
    transfers: HashMap<Ty, bool>,
    glues: HashMap<Glue, FuncId>,
    /// The number of glue functions and tables named so far.
    glue: u32,
    state_id: Option<DataId>,
    thread_id: Option<DataId>,
    pub data: data::Data,
    pub lines: lines::Lines,
    /// The task entries whose own `limits stack N` passes the default task stack, by the code a
    /// function value naming them holds, and the table of them that spawns read.
    pub entry_stacks: Vec<(FuncId, u64)>,
    pub entry_table: Option<DataId>,
    frames: Vec<Frame>,
    pub demands: Vec<Demand>,
    /// For a test image, what its tests' runs need, and the sim table once declared.
    test: Option<test_image::Runtime>,
    sims: Option<DataId>,
}

/// A function declared and not yet defined.
enum Pending {
    Instance(Instance, FuncId),
    Routine(runtime::Routine, FuncId),
}

/// The settings every image's code is generated with: the x86-64 base set with SSE2, frame
/// pointers in every function, stack probes inline, stack switching, and the canonical NaN as
/// the NaN every float operation returns.
fn isa() -> OwnedTargetIsa {
    let mut flags = settings::builder();
    for (name, value) in [
        ("opt_level", "speed"),
        ("is_pic", "true"),
        ("preserve_frame_pointers", "true"),
        ("stack_switch_model", "basic"),
        ("enable_probestack", "true"),
        ("probestack_strategy", "inline"),
        ("unwind_info", "false"),
        ("tls_model", "elf_gd"),
        ("enable_nan_canonicalization", "true"),
    ] {
        flags.set(name, value).expect("a Cranelift setting");
    }
    cranelift_codegen::isa::lookup_by_name("x86_64-unknown-linux-gnu")
        .expect("the x86-64 backend")
        .finish(settings::Flags::new(flags))
        .expect("the x86-64 settings")
}

/// Lowers the reach of a program's final build.
pub fn lower(ck: &Checker, program: &Program) -> Lowered {
    let mut lowering = Lowering::new(ck, program.prefixes.clone(), program.roots.clone(), None);
    host::entry_seam(&mut lowering, program);
    lowering.drain();
    lowering.finish()
}

impl<'c, 'a> Lowering<'c, 'a> {
    /// A lowering into a new object, of a test image when `test` is given.
    fn new(
        ck: &'c Checker<'a>,
        prefixes: HashMap<String, String>,
        roots: Vec<PackageId>,
        test: Option<test_image::Runtime>,
    ) -> Self {
        let isa = isa();
        let builder = ObjectBuilder::new(
            isa.clone(),
            "nan",
            cranelift_module::default_libcall_names(),
        )
        .expect("an object builder");
        Lowering {
            ck,
            module: ObjectModule::new(builder),
            layouts: Layouts::new(),
            isa,
            prefixes,
            roots,
            instances: HashMap::new(),
            routines: HashMap::new(),
            queue: Vec::new(),
            releases: HashMap::new(),
            transfers: HashMap::new(),
            glues: HashMap::new(),
            glue: 0,
            state_id: None,
            thread_id: None,
            data: data::Data::default(),
            lines: lines::Lines::default(),
            entry_stacks: Vec::new(),
            entry_table: None,
            frames: Vec::new(),
            demands: Vec::new(),
            test,
            sims: None,
        }
    }

    /// The function lowering an instance, declared now and lowered later when new.
    pub fn instance(&mut self, instance: Instance) -> FuncId {
        if let Some(id) = self.instances.get(&instance) {
            return *id;
        }
        let abi = self.abi(&instance);
        let name = symbols::name(self, &instance);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &abi.signature())
            .unwrap_or_else(|error| panic!("declaring {name}: {error}"));
        self.instances.insert(instance.clone(), id);
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
                        Routine::TaskStart => lines::HIDDEN,
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

    /// Names the frame of an instance just defined and the `stack N` its function-typed
    /// parameters' types demand, and records the demands of its own limits and of the interface
    /// method it implements.
    fn describe_frame(&mut self, instance: &Instance, abi: &Abi, id: FuncId) {
        let frame = self.frames.last_mut().expect("the frame just defined");
        frame.params = abi
            .params
            .iter()
            .enumerate()
            .filter_map(|(position, (nty, _))| match nty.ty() {
                Ty::Fn(fn_ty) => Some((position, fn_ty.limits.stack)),
                _ => None,
            })
            .collect();
        let item = match instance {
            Instance::Nir { item, .. } | Instance::Supplied { item, .. } => *item,
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
                    && let crate::check::sig::SigKind::Interface { methods } =
                        &self.ck.sig(implemented.interface).kind
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
    /// `task::Receiver`, `task::Scope`, `sys::Cx`, `sys::Outbox`, `mem::Shared` or
    /// `task::Cown`.
    fn frame_flags(&self, instance: &Instance) -> u32 {
        let (hidden, critical) = match instance {
            Instance::Nir { item, args, .. } | Instance::Supplied { item, args } => {
                let std = self.ck.db.is_std(*item);
                (std, std && self.critical_owner(*item, args))
            }
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
        let recorded = {
            let mut fx = Fx::new(FunctionBuilder::new(
                &mut context.func,
                &mut builder_context,
            ));
            build(self, &mut fx);
            fx.b.seal_all_blocks();
            let recorded = std::mem::take(&mut fx.sites);
            fx.b.finalize(self.isa.frontend_config());
            recorded
        };
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
    /// direct call of a routine or the C library bounded by the stub call bound, of another
    /// lowered function direct, and an unclassified call through an address unbounded.
    fn sites(
        &self,
        func: &ir::Function,
        recorded: Vec<(ir::Inst, stack::Site)>,
    ) -> Vec<stack::Site> {
        let mut recorded: HashMap<ir::Inst, stack::Site> = recorded.into_iter().collect();
        let mut sites = Vec::new();
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                if let Some(site) = recorded.remove(&inst) {
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
                        if declaration.linkage == Linkage::Import
                            || self.routines.values().any(|routine| *routine == callee)
                        {
                            stack::Kind::Stub
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
        self.glues.insert(glue, id);
        self.define(id, signature, flags, build);
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

    fn finish(mut self) -> Lowered {
        lines::emit(&mut self);
        runtime::tasks::emit_entry_stacks(&mut self);
        let frames = std::mem::take(&mut self.frames);
        let demands = std::mem::take(&mut self.demands);
        let product = self.module.finish();
        Lowered {
            object: product.emit().expect("an object file"),
            frames,
            demands,
        }
    }
}

/// The calling convention of every lowered function.
pub const CALL_CONV: CallConv = CallConv::SystemV;
