//! Line tables: the source location of each instruction of lowered code, by function and code
//! offset, which trap reports read to name the trapping operation and each caller.
//!
//! An image's line tables are its data object `_NSlines`: the addresses of the function, entry
//! and location arrays and of the table of task entries' own stacks, the function count, and the
//! address of the line tables of the next bundle loaded, zero for none. A function record is its
//! address, its size, its first entry, its entry count and its flags; an entry is a code offset
//! and a location; a location is the offsets from the record and lengths of its path, item and
//! source line texts, which follow the records in the same array, and its line number.

use std::collections::HashMap;

use cranelift_codegen::ir::{InstBuilder, Value};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};

use super::Lowering;
use super::fx::Fx;
use super::load::Linking;
use crate::check::Checker;
use crate::check::db::ItemId;
use crate::nir::Span;

/// The symbol of the line tables, and of a bundle's own, which the walk from the kernel's reaches.
pub const SYMBOL: &str = "_NSlines";
pub const BUNDLE_SYMBOL: &str = "_NSbundle_lines";

/// The offsets in line tables of the location array, of the table of task entries' own stacks,
/// of the function count and of the next line tables, and their size.
pub const LOCATIONS: u64 = 16;
pub const ENTRY_STACKS: u64 = 24;
pub const COUNT: u64 = 32;
pub const NEXT: u64 = 40;
pub const HEADER: u64 = 48;

/// The flags of a function record: trap reports leave its frames out, and a stopping task
/// does not stop while one of its frames is on the task's stack.
pub const HIDDEN: u32 = 1;
pub const CRITICAL: u32 = 2;

/// The size of a function record, an entry and a location.
pub const FUNCTION_RECORD: u64 = 24;
pub const ENTRY_RECORD: u64 = 8;
pub const LOCATION_RECORD: u64 = 28;

#[derive(Default)]
pub struct Lines {
    locations: Vec<Location>,
    known: HashMap<(ItemId, u32), u32>,
    functions: Vec<Function>,
    table: Option<DataId>,
    array: Option<DataId>,
}

struct Location {
    path: String,
    line: u32,
    item: String,
    text: String,
}

struct Function {
    id: FuncId,
    size: u32,
    flags: u32,
    entries: Vec<(u32, u32)>,
}

impl Lines {
    /// The number of a span's location, as source locations of lowered code carry it.
    pub fn location(&mut self, ck: &Checker, item: ItemId, span: Span) -> u32 {
        let data = ck.db.item(item);
        let line = data.line(span.first);
        if let Some(known) = self.known.get(&(item, line)) {
            return *known;
        }
        let text = String::from_utf8_lossy(data.file.syntax.line_text(line))
            .trim_start()
            .to_owned();
        self.locations.push(Location {
            path: path(ck, item),
            line,
            item: ck
                .canonical_path(item)
                .unwrap_or_else(|| ck.db.canonical(item)),
            text,
        });
        let number = self.locations.len() as u32;
        self.known.insert((item, line), number);
        number
    }

    /// Records a compiled function's size and the locations of its code.
    pub fn function(&mut self, id: FuncId, flags: u32, size: u32, entries: Vec<(u32, u32)>) {
        self.functions.push(Function {
            id,
            size,
            flags,
            entries,
        });
    }

    /// The data object of the image's line tables, declared once and defined when lowering
    /// finishes: writable in a kernel and in a user-mode runtime, whose next field loading
    /// links, and named apart in a bundle, which imports the kernel's.
    pub fn table(
        &mut self,
        module: &mut cranelift_object::ObjectModule,
        linking: &Linking,
    ) -> DataId {
        *self.table.get_or_insert_with(|| {
            let (name, writable) = match linking {
                Linking::Image => (SYMBOL, false),
                Linking::Kernel(_) | Linking::Runtime => (SYMBOL, true),
                Linking::Bundle => (BUNDLE_SYMBOL, false),
            };
            module
                .declare_data(name, Linkage::Local, writable, false)
                .expect("the line tables")
        })
    }

    /// The location array, declared once and defined when lowering finishes.
    fn array(&mut self, module: &mut cranelift_object::ObjectModule) -> DataId {
        *self.array.get_or_insert_with(|| {
            module
                .declare_data(&format!("{SYMBOL}_locations"), Linkage::Local, false, false)
                .expect("the location array")
        })
    }
}

impl Lowering<'_, '_> {
    /// The address of the record of a location numbered from 1, zero for 0.
    pub fn location_address(&mut self, fx: &mut Fx, number: u32) -> Value {
        if number == 0 {
            return fx.ptr(0);
        }
        let array = self.lines.array(&mut self.module);
        let base = self.data_address(fx, array);
        fx.b.ins()
            .iadd_imm_s(base, i64::from(number - 1) * LOCATION_RECORD as i64)
    }

    /// The address of the line tables a walk starts from: on a bundle the kernel's, which it
    /// imports, on any other image its own.
    pub fn line_head(&mut self, fx: &mut Fx) -> Value {
        let id = match self.imports() {
            true => self
                .module
                .declare_data(SYMBOL, Linkage::Import, true, false)
                .expect("the kernel's line tables"),
            false => self.lines.table(&mut self.module, &self.linking),
        };
        self.data_address(fx, id)
    }
}

/// The path of an item's file as reports write it: its package's name, then the path within
/// the package's source directory.
pub fn path(ck: &Checker, item: ItemId) -> String {
    let file = &ck.db.item(item).file.path;
    let package = ck.db.package(ck.db.package_of(item));
    let relative = file
        .strip_prefix(&package.source.dir)
        .map(|rest| rest.trim_start_matches('/'))
        .unwrap_or(file);
    format!("{}/{relative}", package.name)
}

/// Defines the line tables from every function lowered, pointing at the table of task entries'
/// own stacks.
pub fn emit(lo: &mut Lowering, entry_stacks: DataId) {
    let table = lo.lines.table(&mut lo.module, &lo.linking);
    let array = lo.lines.array(&mut lo.module);
    let lines = std::mem::take(&mut lo.lines);
    let mut strings: Vec<u8> = Vec::new();
    let mut text = |text: &str| {
        let at = strings.len() as u32;
        strings.extend_from_slice(text.as_bytes());
        (at, text.len() as u32)
    };
    let mut texts = Vec::new();
    for location in &lines.locations {
        texts.push([
            text(&location.path),
            text(&location.item),
            text(&location.text),
        ]);
    }
    let records = lines.locations.len() as u32 * LOCATION_RECORD as u32;
    let mut locations = Vec::new();
    for (index, (location, texts)) in lines.locations.iter().zip(&texts).enumerate() {
        let from_record = records - index as u32 * LOCATION_RECORD as u32;
        for (at, len) in texts {
            locations.extend_from_slice(&(from_record + at).to_le_bytes());
            locations.extend_from_slice(&len.to_le_bytes());
        }
        locations.extend_from_slice(&location.line.to_le_bytes());
    }
    locations.extend_from_slice(&strings);
    let mut entries = Vec::new();
    let mut functions = Vec::new();
    let mut function_relocs = Vec::new();
    for function in &lines.functions {
        let first = (entries.len() / ENTRY_RECORD as usize) as u32;
        for (offset, location) in &function.entries {
            entries.extend_from_slice(&offset.to_le_bytes());
            entries.extend_from_slice(&location.to_le_bytes());
        }
        function_relocs.push((functions.len() as u32, function.id));
        functions.extend_from_slice(&0u64.to_le_bytes());
        functions.extend_from_slice(&function.size.to_le_bytes());
        functions.extend_from_slice(&first.to_le_bytes());
        functions.extend_from_slice(&(function.entries.len() as u32).to_le_bytes());
        functions.extend_from_slice(&function.flags.to_le_bytes());
    }
    let mut arrays = Vec::new();
    for (index, bytes) in [functions, entries].into_iter().enumerate() {
        let id = lo
            .module
            .declare_data(&format!("{SYMBOL}_{index}"), Linkage::Local, false, false)
            .expect("a line table array");
        let mut description = DataDescription::new();
        let mut bytes = bytes;
        if bytes.is_empty() {
            bytes.push(0);
        }
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        if index == 0 {
            for (offset, function) in &function_relocs {
                let reference = lo.module.declare_func_in_data(*function, &mut description);
                description.write_function_addr(*offset, reference);
            }
        }
        lo.module
            .define_data(id, &description)
            .expect("a line table array");
        arrays.push(id);
    }
    let mut description = DataDescription::new();
    if locations.is_empty() {
        locations.push(0);
    }
    description.define(locations.into_boxed_slice());
    description.set_align(8);
    lo.module
        .define_data(array, &description)
        .expect("the location array");
    arrays.push(array);
    arrays.push(entry_stacks);
    let mut header = vec![0u8; HEADER as usize];
    header[COUNT as usize..COUNT as usize + 4]
        .copy_from_slice(&(lines.functions.len() as u32).to_le_bytes());
    let mut description = DataDescription::new();
    description.define(header.into_boxed_slice());
    description.set_align(8);
    for (index, array) in arrays.iter().enumerate() {
        let reference = lo.module.declare_data_in_data(*array, &mut description);
        description.write_data_addr(8 * index as u32, reference, 0);
    }
    lo.module
        .define_data(table, &description)
        .expect("the line tables");
}
