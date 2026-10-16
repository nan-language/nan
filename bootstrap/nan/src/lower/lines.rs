//! Line tables: the source location of each instruction of lowered code, by function and code
//! offset, which trap reports read to name the trapping operation and each caller.
//!
//! The table is the data object `_NSlines`: the addresses of the function, entry, location and
//! string arrays and their counts. A function record is its address, its size, its first entry,
//! its entry count and its flags; an entry is a code offset and a location; a location is the
//! offsets and lengths of its path, item and source line texts, and its line number.

use std::collections::HashMap;

use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};

use super::Lowering;
use crate::check::Checker;
use crate::check::db::ItemId;
use crate::nir::Span;

/// The symbol of the line tables.
pub const SYMBOL: &str = "_NSlines";

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
        let package = ck.db.package(ck.db.package_of(item));
        let relative = data
            .file
            .path
            .strip_prefix(&package.source.dir)
            .map(|rest| rest.trim_start_matches('/'))
            .unwrap_or(&data.file.path);
        let text = String::from_utf8_lossy(data.file.syntax.line_text(line))
            .trim_start()
            .to_owned();
        self.locations.push(Location {
            path: format!("{}/{relative}", package.name),
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

    /// The data object of the line tables, declared once and defined when lowering finishes.
    pub fn table(&mut self, module: &mut cranelift_object::ObjectModule) -> DataId {
        *self.table.get_or_insert_with(|| {
            module
                .declare_data(SYMBOL, Linkage::Local, false, false)
                .expect("the line tables")
        })
    }
}

/// Defines the line tables from every function lowered.
pub fn emit(lo: &mut Lowering) {
    let table = lo.lines.table(&mut lo.module);
    let lines = std::mem::take(&mut lo.lines);
    let mut strings: Vec<u8> = Vec::new();
    let text = |text: &str, strings: &mut Vec<u8>| {
        let at = strings.len() as u32;
        strings.extend_from_slice(text.as_bytes());
        (at, text.len() as u32)
    };
    let mut locations = Vec::new();
    for location in &lines.locations {
        let (path, path_len) = text(&location.path, &mut strings);
        let (item, item_len) = text(&location.item, &mut strings);
        let (line_text, line_len) = text(&location.text, &mut strings);
        for value in [
            path,
            path_len,
            item,
            item_len,
            line_text,
            line_len,
            location.line,
        ] {
            locations.extend_from_slice(&value.to_le_bytes());
        }
    }
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
    let arrays: Vec<DataId> = [functions, entries, locations, strings]
        .into_iter()
        .enumerate()
        .map(|(index, bytes)| {
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
            id
        })
        .collect();
    let mut header = vec![0u8; 40];
    header[32..36].copy_from_slice(&(lines.functions.len() as u32).to_le_bytes());
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
