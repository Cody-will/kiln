//! DWARF line tables for Cranelift builds (debug builds on ELF targets).
//!
//! Every MIR statement and terminator sets a Cranelift source location: an
//! index into [`DebugInfo::spans`], the span's global start offset. After a
//! function is compiled, its machine-code offset → location table is kept.
//! [`DebugInfo::emit`] then writes a DWARF 4 compile unit (one subprogram
//! per function, with a line program) into the object file, with
//! relocations against the function symbols, so `gdb`, `lldb`, `perf`, and
//! sanitizer backtraces show `file.kn:line`.

use cranelift_codegen::ir::SourceLoc;
use cranelift_module::FuncId;
use gimli::write::{
    Address, AttributeValue, DwarfUnit, EndianVec, LineProgram, LineString, Range, RangeList,
    Sections, Writer,
};
use gimli::{Encoding, Format, LineEncoding, RunTimeEndian, SectionId};
use object::write::{Object, Relocation, SymbolId};
use object::{RelocationEncoding, RelocationFlags, RelocationKind, SectionKind};
use std::collections::HashMap;

/// A compiled function's line information.
struct FnLines {
    id: FuncId,
    name: String,
    size: u32,
    /// (code offset, span start), sorted by offset.
    rows: Vec<(u32, usize)>,
}

#[derive(Default)]
pub(crate) struct DebugInfo {
    spans: Vec<usize>,
    index: HashMap<usize, u32>,
    funcs: Vec<FnLines>,
}

impl DebugInfo {
    /// The source location for a span starting at `start`.
    pub(crate) fn loc(&mut self, start: usize) -> SourceLoc {
        let n = self.spans.len() as u32;
        let i = *self.index.entry(start).or_insert(n);
        if i == n {
            self.spans.push(start);
        }
        SourceLoc::new(i)
    }

    /// Records a compiled function's source locations.
    pub(crate) fn add_function(
        &mut self,
        id: FuncId,
        name: &str,
        ctx: &cranelift_codegen::Context,
    ) {
        let Some(cc) = ctx.compiled_code() else {
            return;
        };
        let size = cc.code_info().total_size;
        let mut rows: Vec<(u32, usize)> = vec![];
        for l in cc.buffer.get_srclocs_sorted() {
            if l.loc.is_default() {
                continue;
            }
            let Some(&start) = self.spans.get(l.loc.bits() as usize) else {
                continue;
            };
            if rows.last().is_some_and(|r| r.1 == start) {
                continue;
            }
            rows.push((l.start, start));
        }
        // The prologue belongs to the function's first line.
        if let Some(r) = rows.first_mut() {
            r.0 = 0;
        }
        if !rows.is_empty() {
            self.funcs.push(FnLines {
                id,
                name: name.to_string(),
                size,
                rows,
            });
        }
    }

    /// Adds `.debug_*` sections to `obj`. `sym` maps a function to its symbol.
    pub(crate) fn emit(
        self,
        obj: &mut Object<'static>,
        sym: impl Fn(FuncId) -> SymbolId,
    ) -> Result<(), String> {
        if self.funcs.is_empty() {
            return Ok(());
        }
        let encoding = Encoding {
            format: Format::Dwarf32,
            version: 4,
            address_size: 8,
        };
        // Files: global span base → (name, line index).
        let mut files: HashMap<usize, (&'static str, kiln_lexer::LineIndex)> = HashMap::new();
        let mut locate = |start: usize| -> Option<(usize, &'static str, usize, usize)> {
            let (base, name, text) = kiln_lexer::source_at(start)?;
            let e = files
                .entry(base)
                .or_insert_with(|| (name, kiln_lexer::LineIndex::new(text)));
            let (line, col) = e.1.line_col(start - base);
            Some((base, e.0, line, col))
        };
        let root = kiln_lexer::source_at(0).map(|x| x.1).unwrap_or("main.kn");
        let cwd = std::env::current_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        let mut program = LineProgram::new(
            encoding,
            LineEncoding::default(),
            LineString::String(cwd.clone().into_bytes()),
            LineString::String(root.as_bytes().to_vec()),
            None,
        );
        let dir = program.default_directory();
        let mut dwarf = DwarfUnit::new(encoding);
        let mut file_ids = HashMap::new();
        let mut ranges = RangeList(vec![]);
        let unit_root = dwarf.unit.root();
        let syms: Vec<SymbolId> = self.funcs.iter().map(|f| sym(f.id)).collect();
        for (k, f) in self.funcs.iter().enumerate() {
            let addr = Address::Symbol {
                symbol: k,
                addend: 0,
            };
            program.begin_sequence(Some(addr));
            let mut first: Option<(gimli::write::FileId, usize)> = None;
            for &(off, start) in &f.rows {
                let Some((base, name, line, col)) = locate(start) else {
                    continue;
                };
                let fid = *file_ids.entry(base).or_insert_with(|| {
                    program.add_file(LineString::String(name.as_bytes().to_vec()), dir, None)
                });
                if first.is_none() {
                    first = Some((fid, line));
                }
                let row = program.row();
                row.address_offset = u64::from(off);
                row.file = fid;
                row.line = line as u64;
                row.column = col as u64;
                program.generate_row();
            }
            program.end_sequence(u64::from(f.size));
            ranges.0.push(Range::StartLength {
                begin: addr,
                length: u64::from(f.size),
            });
            let sp = dwarf.unit.add(unit_root, gimli::DW_TAG_subprogram);
            let e = dwarf.unit.get_mut(sp);
            e.set(
                gimli::DW_AT_name,
                AttributeValue::String(f.name.clone().into_bytes()),
            );
            e.set(gimli::DW_AT_low_pc, AttributeValue::Address(addr));
            e.set(
                gimli::DW_AT_high_pc,
                AttributeValue::Udata(u64::from(f.size)),
            );
            if let Some((fid, line)) = first {
                e.set(gimli::DW_AT_decl_file, AttributeValue::FileIndex(Some(fid)));
                e.set(gimli::DW_AT_decl_line, AttributeValue::Udata(line as u64));
            }
        }
        dwarf.unit.line_program = program;
        let rl = dwarf.unit.ranges.add(ranges);
        let cu = dwarf.unit.get_mut(unit_root);
        cu.set(
            gimli::DW_AT_producer,
            AttributeValue::String(format!("kiln {}", env!("CARGO_PKG_VERSION")).into_bytes()),
        );
        cu.set(
            gimli::DW_AT_name,
            AttributeValue::String(root.as_bytes().to_vec()),
        );
        cu.set(
            gimli::DW_AT_comp_dir,
            AttributeValue::String(cwd.into_bytes()),
        );
        // No Kiln language code exists; C lets debuggers show lines and frames.
        cu.set(
            gimli::DW_AT_language,
            AttributeValue::Language(gimli::DW_LANG_C99),
        );
        cu.set(
            gimli::DW_AT_low_pc,
            AttributeValue::Address(Address::Constant(0)),
        );
        cu.set(gimli::DW_AT_ranges, AttributeValue::RangeListRef(rl));

        let mut sections = Sections::new(W::new());
        dwarf
            .write(&mut sections)
            .map_err(|e| format!("debug info: {e}"))?;
        // Create every non-empty section first, so section relocations resolve.
        let mut ids: HashMap<SectionId, object::write::SectionId> = HashMap::new();
        sections.for_each(|id, w| -> Result<(), String> {
            if !w.inner.slice().is_empty() {
                let s = obj.add_section(vec![], id.name().as_bytes().to_vec(), SectionKind::Debug);
                obj.append_section_data(s, w.inner.slice(), 1);
                ids.insert(id, s);
            }
            Ok(())
        })?;
        sections.for_each(|id, w| -> Result<(), String> {
            let Some(&sec) = ids.get(&id) else {
                return Ok(());
            };
            for r in &w.relocs {
                let (symbol, addend) = match r.target {
                    Target::Symbol(k) => (syms[k], r.addend),
                    Target::Section(s) => match ids.get(&s) {
                        Some(&t) => (obj.section_symbol(t), r.addend),
                        None => continue,
                    },
                };
                obj.add_relocation(
                    sec,
                    Relocation {
                        offset: u64::from(r.offset),
                        symbol,
                        addend,
                        flags: RelocationFlags::Generic {
                            kind: RelocationKind::Absolute,
                            encoding: RelocationEncoding::Generic,
                            size: r.size * 8,
                        },
                    },
                )
                .map_err(|e| format!("debug info relocation: {e}"))?;
            }
            Ok(())
        })?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Target {
    Symbol(usize),
    Section(SectionId),
}

#[derive(Clone)]
struct Reloc {
    offset: u32,
    size: u8,
    target: Target,
    addend: i64,
}

/// A section writer that records relocations instead of absolute values.
#[derive(Clone)]
struct W {
    inner: EndianVec<RunTimeEndian>,
    relocs: Vec<Reloc>,
}

impl W {
    fn new() -> W {
        W {
            inner: EndianVec::new(RunTimeEndian::Little),
            relocs: vec![],
        }
    }
}

impl Writer for W {
    type Endian = RunTimeEndian;

    fn endian(&self) -> Self::Endian {
        self.inner.endian()
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn write(&mut self, bytes: &[u8]) -> gimli::write::Result<()> {
        self.inner.write(bytes)
    }

    fn write_at(&mut self, offset: usize, bytes: &[u8]) -> gimli::write::Result<()> {
        self.inner.write_at(offset, bytes)
    }

    fn write_address(&mut self, address: Address, size: u8) -> gimli::write::Result<()> {
        match address {
            Address::Constant(v) => self.write_udata(v, size),
            Address::Symbol { symbol, addend } => {
                self.relocs.push(Reloc {
                    offset: self.len() as u32,
                    size,
                    target: Target::Symbol(symbol),
                    addend,
                });
                self.write_udata(0, size)
            }
        }
    }

    fn write_offset(
        &mut self,
        val: usize,
        section: SectionId,
        size: u8,
    ) -> gimli::write::Result<()> {
        self.relocs.push(Reloc {
            offset: self.len() as u32,
            size,
            target: Target::Section(section),
            addend: val as i64,
        });
        self.write_udata(0, size)
    }

    fn write_offset_at(
        &mut self,
        offset: usize,
        val: usize,
        section: SectionId,
        size: u8,
    ) -> gimli::write::Result<()> {
        self.relocs.push(Reloc {
            offset: offset as u32,
            size,
            target: Target::Section(section),
            addend: val as i64,
        });
        self.write_udata_at(offset, 0, size)
    }
}
