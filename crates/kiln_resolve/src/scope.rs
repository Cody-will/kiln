//! The resolver: item collection, import binding, and scoped body walking.

use crate::*;
use std::collections::HashSet;

/// An entry in a block scope.
#[derive(Clone, Copy)]
enum Entry {
    Local(LocalId),
    Def(DefId),
}

#[derive(Default)]
struct Scope {
    values: HashMap<String, Entry>,
    types: HashMap<String, DefId>,
}

/// Which namespace a path is looked up in.
#[derive(Clone, Copy, PartialEq)]
enum Ns {
    Type,
    Value,
}

/// Loop context: labels of enclosing loops, reset at closure/`go` boundaries.
type LoopStack = Vec<Option<String>>;

pub(crate) struct Resolver<'a> {
    r: Resolved<'a>,
    module: DefId,
    scopes: Vec<Scope>,
    ty_params: Vec<Vec<String>>,
    self_ty_allowed: bool,
    self_local: Option<LocalId>,
    loops: LoopStack,
    in_prelude: bool,
}

impl<'a> Resolver<'a> {
    pub(crate) fn run(file: &'a File, prelude: &'a File) -> Resolved<'a> {
        let mut s = Resolver {
            r: Resolved {
                defs: Vec::new(),
                root: DefId(0),
                prelude: DefId(0),
                modules: HashMap::new(),
                paths: HashMap::new(),
                bindings: HashMap::new(),
                locals: Vec::new(),
                impls: Vec::new(),
                fns: Vec::new(),
                statics: Vec::new(),
                const_fns: HashMap::new(),
                const_fn_set: HashSet::new(),
                errors: Vec::new(),
            },
            module: DefId(0),
            scopes: Vec::new(),
            ty_params: Vec::new(),
            self_ty_allowed: false,
            self_local: None,
            loops: Vec::new(),
            in_prelude: false,
        };
        // Prelude module.
        let prelude_id = s.new_module("<prelude>", None, true);
        s.r.prelude = prelude_id;
        s.in_prelude = true;
        s.collect_items(&prelude.items, prelude_id);
        // Prelude variants of Option/Result are values (`Some`, `None`, `Ok`, `Err`).
        for name in ["Option", "Result"] {
            let id = s.r.modules[&prelude_id].types[name];
            for v in s.r.variants(id) {
                let vname = s.r.def(v).name.clone();
                s.r.modules
                    .get_mut(&prelude_id)
                    .unwrap()
                    .values
                    .insert(vname, v);
            }
        }
        // User root module.
        s.in_prelude = false;
        let root = s.new_module("<root>", None, false);
        s.r.root = root;
        s.collect_items(&file.items, root);
        s.in_prelude = true;
        s.bind_imports(&prelude.items, prelude_id);
        s.in_prelude = false;
        s.bind_imports(&file.items, root);
        // Walk everything.
        s.in_prelude = true;
        s.walk_items(&prelude.items, prelude_id);
        s.in_prelude = false;
        s.walk_items(&file.items, root);
        s.r
    }

    fn error(&mut self, msg: impl Into<String>, span: Span) {
        self.r.errors.push(Diagnostic::error(msg, span));
    }

    fn error_help(&mut self, msg: impl Into<String>, span: Span, help: impl Into<String>) {
        self.r
            .errors
            .push(Diagnostic::error(msg, span).with_help(help));
    }

    fn new_def(&mut self, kind: DefKind, name: &Ident, vis: Visibility, module: DefId) -> DefId {
        let id = DefId(self.r.defs.len() as u32);
        self.r.defs.push(Def {
            kind,
            name: name.name.clone(),
            span: name.span,
            vis,
            module,
            item: None,
            variant: None,
            is_prelude: self.in_prelude,
        });
        id
    }

    fn new_module(&mut self, name: &str, parent: Option<DefId>, prelude: bool) -> DefId {
        let id = DefId(self.r.defs.len() as u32);
        self.r.defs.push(Def {
            kind: DefKind::Mod,
            name: name.to_string(),
            span: Span::default(),
            vis: Visibility::Public,
            module: parent.unwrap_or(id),
            item: None,
            variant: None,
            is_prelude: prelude,
        });
        self.r.modules.insert(
            id,
            ModuleScope {
                parent,
                ..Default::default()
            },
        );
        id
    }

    fn insert(&mut self, module: DefId, ns: Ns, name: &Ident, id: DefId) {
        let scope = self.r.modules.get_mut(&module).unwrap();
        let table = match ns {
            Ns::Type => &mut scope.types,
            Ns::Value => &mut scope.values,
        };
        if table.contains_key(&name.name) {
            let what = if ns == Ns::Type { "type" } else { "value" };
            self.error(
                format!(
                    "the name `{}` is defined multiple times in this module ({what} namespace)",
                    name.name
                ),
                name.span,
            );
            return;
        }
        table.insert(name.name.clone(), id);
    }

    // ----- phase 1: collect items ------------------------------------------

    /// Creates definitions for `items`, returning `(name, ns, id)` triples so
    /// the caller can insert them into a module or block scope.
    fn define_items(&mut self, items: &'a [Item], module: DefId) -> Vec<(Ident, Ns, DefId)> {
        let mut out = Vec::new();
        for item in items {
            let (kind, name) = match &item.kind {
                ItemKind::Fn(f) => (DefKind::Fn, &f.name),
                ItemKind::Struct(s) => (DefKind::Struct, &s.name),
                ItemKind::Enum(e) => (DefKind::Enum, &e.name),
                ItemKind::Trait(t) => (DefKind::Trait, &t.name),
                ItemKind::Const { name, .. } => (DefKind::Const, name),
                ItemKind::Static { name, .. } => (DefKind::Static, name),
                ItemKind::TypeAlias { name, .. } => (DefKind::TypeAlias, name),
                ItemKind::Mod { name, items: body } => {
                    let id = self.new_module(&name.name, Some(module), self.in_prelude);
                    let d = &mut self.r.defs[id.0 as usize];
                    d.name = name.name.clone();
                    d.span = name.span;
                    d.vis = item.vis;
                    d.item = Some(item);
                    out.push((name.clone(), Ns::Type, id));
                    match body {
                        Some(items) => self.collect_items(items, id),
                        None => self.error(
                            format!("out-of-line module `{}` is not supported yet; write `mod {} {{ ... }}`", name.name, name.name),
                            name.span,
                        ),
                    }
                    continue;
                }
                ItemKind::Impl(decl) => {
                    self.r.impls.push(ImplRef {
                        decl,
                        span: item.span,
                        module,
                        is_prelude: self.in_prelude,
                    });
                    continue;
                }
                ItemKind::Import(_) => continue,
            };
            let id = self.new_def(kind, name, item.vis, module);
            self.r.defs[id.0 as usize].item = Some(item);
            match &item.kind {
                ItemKind::Fn(_) | ItemKind::Const { .. } | ItemKind::Static { .. } => {
                    out.push((name.clone(), Ns::Value, id))
                }
                ItemKind::Struct(s) => {
                    out.push((name.clone(), Ns::Type, id));
                    if !matches!(s.fields, Fields::Named(_)) {
                        out.push((name.clone(), Ns::Value, id));
                    }
                }
                ItemKind::Enum(e) => {
                    out.push((name.clone(), Ns::Type, id));
                    let mut seen = HashSet::new();
                    for (index, v) in e.variants.iter().enumerate() {
                        if !seen.insert(v.name.name.clone()) {
                            self.error(
                                format!("variant `{}` is defined multiple times", v.name.name),
                                v.name.span,
                            );
                        }
                        let vid = self.new_def(
                            DefKind::Variant { parent: id, index },
                            &v.name,
                            Visibility::Public,
                            module,
                        );
                        self.r.defs[vid.0 as usize].variant = Some(v);
                    }
                }
                _ => out.push((name.clone(), Ns::Type, id)),
            }
        }
        out
    }

    fn collect_items(&mut self, items: &'a [Item], module: DefId) {
        for (name, ns, id) in self.define_items(items, module) {
            self.insert(module, ns, &name, id);
        }
    }

    // ----- phase 2: imports --------------------------------------------------

    fn bind_imports(&mut self, items: &'a [Item], module: DefId) {
        for item in items {
            match &item.kind {
                ItemKind::Import(tree) => {
                    let public = item.vis != Visibility::Private;
                    self.bind_import(tree, &[], module, public)
                }
                ItemKind::Mod {
                    name,
                    items: Some(body),
                } => {
                    let id = self.r.modules[&module].types[&name.name];
                    self.bind_imports(body, id);
                }
                _ => {}
            }
        }
    }

    /// Binds an imported name; a plain `import` stays private to the module.
    fn bind_imported(&mut self, module: DefId, ns: Ns, name: &Ident, id: DefId, public: bool) {
        self.insert(module, ns, name, id);
        if !public {
            self.r
                .modules
                .get_mut(&module)
                .unwrap()
                .private_imports
                .insert((ns == Ns::Type, name.name.clone()));
        }
    }

    /// Whether code in module `from` can see private names of module `target`.
    fn sees(&self, from: DefId, target: DefId) -> bool {
        let mut m = Some(from);
        while let Some(id) = m {
            if id == target {
                return true;
            }
            m = self.r.modules[&id].parent;
        }
        false
    }

    /// Whether `name` in `module` is a private import hidden from `from`.
    fn hidden_import(&self, module: DefId, is_type: bool, name: &str, from: DefId) -> bool {
        self.r.modules[&module]
            .private_imports
            .contains(&(is_type, name.to_string()))
            && !self.sees(from, module)
    }

    fn bind_import(&mut self, tree: &ImportTree, prefix: &[Ident], module: DefId, public: bool) {
        let full = |p: &[Ident]| -> Vec<Ident> { prefix.iter().chain(p).cloned().collect() };
        match tree {
            ImportTree::Path { path, alias } => {
                let path = full(path);
                // `prefix::{self}` imports the prefix itself.
                let (path, bind_name) =
                    if path.last().is_some_and(|s| s.name == "self") && path.len() > 1 {
                        let p = path[..path.len() - 1].to_vec();
                        let n = alias.clone().unwrap_or_else(|| p.last().unwrap().clone());
                        (p, n)
                    } else {
                        let n = alias
                            .clone()
                            .unwrap_or_else(|| path.last().unwrap().clone());
                        (path, n)
                    };
                if path[0].name == "core" {
                    let id =
                        self.new_def(DefKind::External, &bind_name, Visibility::Private, module);
                    self.bind_imported(module, Ns::Type, &bind_name, id, public);
                    self.bind_imported(module, Ns::Value, &bind_name, id, public);
                    return;
                }
                let (types, values) = self.lookup_import_path(&path, module);
                if types.is_none() && values.is_none() {
                    let s = path
                        .iter()
                        .map(|i| i.name.as_str())
                        .collect::<Vec<_>>()
                        .join("::");
                    let span = path[0].span.to(path.last().unwrap().span);
                    self.error(format!("unresolved import `{s}`"), span);
                    return;
                }
                if let Some(t) = types {
                    self.bind_imported(module, Ns::Type, &bind_name, t, public);
                }
                if let Some(v) = values {
                    self.bind_imported(module, Ns::Value, &bind_name, v, public);
                }
            }
            ImportTree::Group { prefix: p, items } => {
                let new_prefix = full(p);
                for t in items {
                    self.bind_import(t, &new_prefix, module, public);
                }
            }
            ImportTree::Glob { prefix: p } => {
                let path = full(p);
                if path[0].name == "core" {
                    return;
                }
                match self.lookup_import_path(&path, module).0 {
                    Some(m) if self.r.def(m).kind == DefKind::Mod => {
                        let scope = self.r.modules[&m].clone();
                        for (is_type, table) in [(true, scope.types), (false, scope.values)] {
                            for (n, id) in table {
                                if self.r.def(id).vis == Visibility::Private
                                    || self.hidden_import(m, is_type, &n, module)
                                {
                                    continue;
                                }
                                let dst = self.r.modules.get_mut(&module).unwrap();
                                let t = if is_type {
                                    &mut dst.types
                                } else {
                                    &mut dst.values
                                };
                                if !t.contains_key(&n) {
                                    t.insert(n.clone(), id);
                                    if !public {
                                        dst.private_imports.insert((is_type, n));
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        let span = path[0].span.to(path.last().unwrap().span);
                        self.error("glob imports require a module path", span);
                    }
                }
            }
        }
    }

    /// Looks up an import path from the current module (then the root); returns
    /// the type-namespace and value-namespace results.
    fn lookup_import_path(&self, path: &[Ident], module: DefId) -> (Option<DefId>, Option<DefId>) {
        let start = if path[0].name == "self" {
            return self.lookup_in_module_path(&path[1..], module, module);
        } else if path[0].name == "super" {
            // `super::x`, `super::super::x`: start from an ancestor module.
            let mut m = module;
            let mut i = 0;
            while i < path.len() && path[i].name == "super" {
                match self.r.modules[&m].parent {
                    Some(p) => m = p,
                    None => return (None, None),
                }
                i += 1;
            }
            return self.lookup_in_module_path(&path[i..], m, module);
        } else if path[0].name == "std" {
            self.r.prelude
        } else if self.r.modules[&module].types.contains_key(&path[0].name)
            || self.r.modules[&module].values.contains_key(&path[0].name)
        {
            module
        } else if !self.r.modules[&self.r.root]
            .types
            .contains_key(&path[0].name)
            && !self.r.modules[&self.r.root]
                .values
                .contains_key(&path[0].name)
            && (self.r.modules[&self.r.prelude]
                .types
                .contains_key(&path[0].name)
                || self.r.modules[&self.r.prelude]
                    .values
                    .contains_key(&path[0].name))
        {
            // A prelude name (`import Display` re-exports it from a module).
            self.r.prelude
        } else {
            self.r.root
        };
        self.lookup_in_module_path(path, start, module)
    }

    fn lookup_in_module_path(
        &self,
        path: &[Ident],
        mut module: DefId,
        from: DefId,
    ) -> (Option<DefId>, Option<DefId>) {
        if path.is_empty() {
            return (Some(module), None);
        }
        for seg in &path[..path.len() - 1] {
            if self.hidden_import(module, true, &seg.name, from) {
                return (None, None);
            }
            match self.r.modules[&module].types.get(&seg.name) {
                Some(&id) if self.r.def(id).kind == DefKind::Mod => module = id,
                Some(&id) if self.r.def(id).kind == DefKind::Enum => {
                    // Importing a variant: `Shape::Circle`.
                    let last = &path[path.len() - 1].name;
                    let v = self
                        .r
                        .variants(id)
                        .into_iter()
                        .find(|v| &self.r.def(*v).name == last);
                    return (v, v);
                }
                _ => return (None, None),
            }
        }
        let last = &path[path.len() - 1].name;
        let s = &self.r.modules[&module];
        let t = s
            .types
            .get(last)
            .copied()
            .filter(|_| !self.hidden_import(module, true, last, from));
        let v = s
            .values
            .get(last)
            .copied()
            .filter(|_| !self.hidden_import(module, false, last, from));
        (t, v)
    }

    // ----- phase 3: walk items -----------------------------------------------

    fn walk_items(&mut self, items: &'a [Item], module: DefId) {
        let saved = self.module;
        self.module = module;
        for item in items {
            self.walk_item(item);
        }
        self.module = saved;
    }

    fn generic_names(g: &Generics) -> Vec<String> {
        g.params
            .iter()
            .filter_map(|p| match p {
                GenericParam::Type { name, .. } | GenericParam::Const { name, .. } => {
                    Some(name.name.clone())
                }
                GenericParam::Lifetime { .. } => None,
            })
            .collect()
    }

    fn walk_generics(&mut self, g: &'a Generics) {
        let names = Self::generic_names(g);
        let mut seen = HashSet::new();
        for p in &g.params {
            if let GenericParam::Type { name, .. } | GenericParam::Const { name, .. } = p {
                if !seen.insert(&name.name) {
                    self.error(
                        format!("generic parameter `{}` is declared twice", name.name),
                        name.span,
                    );
                }
            }
        }
        self.ty_params.push(names);
        for p in &g.params {
            match p {
                GenericParam::Type {
                    bounds, default, ..
                } => {
                    self.walk_bounds(bounds);
                    if let Some(t) = default {
                        self.walk_type(t);
                    }
                }
                GenericParam::Const { ty, .. } => self.walk_type(ty),
                GenericParam::Lifetime { .. } => {}
            }
        }
        for w in &g.where_clause {
            self.walk_type(&w.ty);
            self.walk_bounds(&w.bounds);
        }
    }

    fn walk_bounds(&mut self, bounds: &'a [Bound]) {
        for b in bounds {
            if let Bound::Trait { path, .. } = b {
                let res = self.resolve_path(path, Ns::Type);
                if let Res::Def(id) = res {
                    if self.r.def(id).kind != DefKind::Trait
                        && self.r.def(id).kind != DefKind::External
                    {
                        self.error(
                            format!(
                                "expected trait, found {} `{}`",
                                kind_name(self.r.def(id).kind),
                                self.r.def(id).name
                            ),
                            path.span,
                        );
                    }
                }
                self.r.paths.insert(path.span, res);
                // `I: Iterator<T>`: resolve the trait's arguments too.
                for seg in &path.segments {
                    self.walk_generic_args(&seg.args);
                }
            }
        }
    }

    fn walk_item(&mut self, item: &'a Item) {
        match &item.kind {
            ItemKind::Fn(f) => {
                let def = self.r.modules[&self.module]
                    .values
                    .get(&f.name.name)
                    .copied()
                    .filter(|d| self.r.def(*d).item.is_some_and(|i| std::ptr::eq(i, item)));
                let def = def.or_else(|| self.find_scoped_def(item));
                self.walk_fn(f, item, None, None, def);
            }
            ItemKind::Struct(s) => {
                self.walk_generics(&s.generics);
                self.walk_fields(&s.fields);
                self.ty_params.pop();
            }
            ItemKind::Enum(e) => {
                self.walk_generics(&e.generics);
                for v in &e.variants {
                    self.walk_fields(&v.fields);
                    if let Some(d) = &v.discriminant {
                        self.walk_expr(d);
                    }
                }
                self.ty_params.pop();
            }
            ItemKind::Trait(t) => {
                let tid = self.r.modules[&self.module]
                    .types
                    .get(&t.name.name)
                    .copied();
                self.walk_generics(&t.generics);
                self.walk_bounds(&t.supertraits);
                let saved = std::mem::replace(&mut self.self_ty_allowed, true);
                for it in &t.items {
                    match &it.kind {
                        ItemKind::Fn(f) => self.walk_fn(f, it, None, tid, None),
                        ItemKind::Const { ty, value, .. } => {
                            self.walk_type(ty);
                            if let Some(v) = value {
                                self.walk_expr(v);
                            }
                        }
                        ItemKind::TypeAlias { ty, .. } => {
                            if let Some(t) = ty {
                                self.walk_type(t);
                            }
                        }
                        _ => self.error(
                            "only `fn`, `const`, and `type` items are allowed in a trait",
                            it.span,
                        ),
                    }
                }
                self.self_ty_allowed = saved;
                self.ty_params.pop();
            }
            ItemKind::Impl(decl) => {
                let idx = self
                    .r
                    .impls
                    .iter()
                    .position(|i| std::ptr::eq(i.decl, decl))
                    .expect("impl was collected");
                self.walk_generics(&decl.generics);
                if let Some(tr) = &decl.trait_ref {
                    let res = self.resolve_path(tr, Ns::Type);
                    if let Res::Def(id) = res {
                        if self.r.def(id).kind != DefKind::Trait {
                            self.error(
                                format!(
                                    "expected trait, found {} `{}`",
                                    kind_name(self.r.def(id).kind),
                                    self.r.def(id).name
                                ),
                                tr.span,
                            );
                        }
                    }
                    self.r.paths.insert(tr.span, res);
                    for seg in &tr.segments {
                        self.walk_generic_args(&seg.args);
                    }
                }
                self.walk_type(&decl.self_ty);
                let saved = std::mem::replace(&mut self.self_ty_allowed, true);
                for it in &decl.items {
                    match &it.kind {
                        ItemKind::Fn(f) => self.walk_fn(f, it, Some(idx), None, None),
                        ItemKind::Const {
                            name,
                            ty,
                            value: Some(v),
                        } => {
                            // `Type::NAME` calls an associated function that
                            // returns the value.
                            let decl = const_fn(name.name.clone(), name, ty, v);
                            self.walk_fn(decl, it, Some(idx), None, None);
                            self.r.const_fn_set.insert(self.r.fns.len() - 1);
                        }
                        ItemKind::Const { ty, .. } => {
                            self.walk_type(ty);
                            self.error("a constant in an `impl` needs a value", it.span);
                        }
                        ItemKind::TypeAlias { ty, .. } => {
                            if let Some(t) = ty {
                                self.walk_type(t);
                            }
                        }
                        _ => self.error(
                            "only `fn`, `const`, and `type` items are allowed in an impl",
                            it.span,
                        ),
                    }
                }
                self.self_ty_allowed = saved;
                self.ty_params.pop();
            }
            ItemKind::Const {
                name,
                ty,
                value: Some(v),
            } => {
                // The value is checked as a hidden function `fn <const X>() -> T`;
                // uses of a non-literal constant call it.
                let def = self.r.modules[&self.module].values.get(&name.name).copied();
                let decl = const_fn(format!("<const {}>", name.name), name, ty, v);
                self.walk_fn(decl, item, None, None, def);
                let i = self.r.fns.len() - 1;
                self.r.const_fn_set.insert(i);
                if let Some(d) = def {
                    self.r.const_fns.insert(d, i);
                }
            }
            ItemKind::Const { ty, .. } => {
                self.walk_type(ty);
                self.error("a constant needs a value", item.span);
            }
            ItemKind::Static {
                name, ty, value, ..
            } => {
                // The initializer becomes a hidden function `fn <init X>() -> T`,
                // checked like any other and run before `main`.
                let def = self.r.modules[&self.module].values.get(&name.name).copied();
                let ret = Expr {
                    kind: ExprKind::Return(Some(Box::new(value.clone()))),
                    span: name.span,
                };
                let decl: &'a FnDecl = Box::leak(Box::new(FnDecl {
                    name: Ident {
                        name: format!("<init {}>", name.name),
                        span: name.span,
                    },
                    is_unsafe: false,
                    generics: Generics::default(),
                    self_param: None,
                    params: vec![],
                    ret: Some(static_lifetimes(ty)),
                    body: Some(Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Expr(ret),
                            span: name.span,
                        }],
                        span: value.span,
                    }),
                }));
                self.walk_fn(decl, item, None, None, def);
                if let Some(d) = def {
                    self.r.statics.push((d, self.r.fns.len() - 1));
                }
            }
            ItemKind::TypeAlias { generics, ty, .. } => {
                self.walk_generics(generics);
                if let Some(t) = ty {
                    self.walk_type(t);
                }
                self.ty_params.pop();
            }
            ItemKind::Mod {
                name,
                items: Some(items),
            } => {
                let id = self.r.modules[&self.module].types[&name.name];
                self.walk_items(items, id);
            }
            ItemKind::Mod { .. } | ItemKind::Import(_) => {}
        }
    }

    fn find_scoped_def(&self, item: &Item) -> Option<DefId> {
        self.r
            .defs
            .iter()
            .position(|d| d.item.is_some_and(|i| std::ptr::eq(i, item)))
            .map(|i| DefId(i as u32))
    }

    fn walk_fields(&mut self, fields: &'a Fields) {
        match fields {
            Fields::Unit => {}
            Fields::Tuple(tys) => tys.iter().for_each(|t| self.walk_type(t)),
            Fields::Named(fs) => {
                let mut seen = HashSet::new();
                for f in fs {
                    if !seen.insert(f.name.name.clone()) {
                        self.error(
                            format!("field `{}` is already declared", f.name.name),
                            f.name.span,
                        );
                    }
                    self.walk_type(&f.ty);
                }
            }
        }
    }

    fn new_local(&mut self, name: &Ident, mutable: bool) -> LocalId {
        let id = LocalId(self.r.locals.len() as u32);
        self.r.locals.push(Local {
            name: name.name.clone(),
            mutable,
            span: name.span,
        });
        self.r.bindings.insert(name.span, id);
        id
    }

    fn declare(&mut self, name: &Ident, mutable: bool) -> LocalId {
        let id = self.new_local(name, mutable);
        self.scopes
            .last_mut()
            .expect("in a body scope")
            .values
            .insert(name.name.clone(), Entry::Local(id));
        id
    }

    fn walk_fn(
        &mut self,
        f: &'a FnDecl,
        item: &'a Item,
        impl_idx: Option<usize>,
        trait_def: Option<DefId>,
        def: Option<DefId>,
    ) {
        self.walk_generics(&f.generics);
        let saved_scopes = std::mem::take(&mut self.scopes);
        let saved_loops = std::mem::take(&mut self.loops);
        let saved_self = self.self_local.take();
        self.scopes.push(Scope::default());
        let in_assoc = impl_idx.is_some() || trait_def.is_some();
        let self_local = match &f.self_param {
            Some(sp) => {
                if !in_assoc {
                    self.error("`self` parameter is only allowed in methods", f.name.span);
                }
                let mutable = matches!(sp, SelfParam::Value { mutable: true });
                let ident = Ident {
                    name: "self".into(),
                    span: f.name.span,
                };
                let id = self.new_local(&ident, mutable);
                self.r.bindings.remove(&f.name.span);
                Some(id)
            }
            None => None,
        };
        self.self_local = self_local;
        for p in &f.params {
            self.walk_type(&p.ty);
        }
        if let Some(t) = &f.ret {
            self.walk_type(t);
        }
        let mut seen = HashSet::new();
        for p in &f.params {
            self.bind_pattern(&p.pat, &mut seen);
        }
        if let Some(body) = &f.body {
            self.walk_block(body);
        } else if impl_idx.is_some() && !self.in_prelude {
            self.error(
                format!("function `{}` in an impl must have a body", f.name.name),
                f.name.span,
            );
        } else if impl_idx.is_none() && trait_def.is_none() && !self.in_prelude {
            self.error(
                format!("function `{}` must have a body", f.name.name),
                f.name.span,
            );
        }
        self.r.fns.push(FnRef {
            decl: f,
            item,
            module: self.module,
            impl_idx,
            trait_def,
            def,
            self_local,
            is_prelude: self.in_prelude,
        });
        self.scopes = saved_scopes;
        self.loops = saved_loops;
        self.self_local = saved_self;
        self.ty_params.pop();
    }

    // ----- types ---------------------------------------------------------------

    fn walk_generic_args(&mut self, args: &'a [GenericArg]) {
        for a in args {
            match a {
                GenericArg::Type(t) | GenericArg::Binding { ty: t, .. } => self.walk_type(t),
                GenericArg::Lifetime(_) => {}
            }
        }
    }

    fn walk_type(&mut self, t: &'a Type) {
        match &t.kind {
            TypeKind::Path(p) => {
                let res = self.resolve_path(p, Ns::Type);
                if let Res::Def(id) = res {
                    let k = self.r.def(id).kind;
                    if matches!(
                        k,
                        DefKind::Fn
                            | DefKind::Const
                            | DefKind::Static
                            | DefKind::Variant { .. }
                            | DefKind::Mod
                    ) {
                        self.error(
                            format!(
                                "expected type, found {} `{}`",
                                kind_name(k),
                                self.r.def(id).name
                            ),
                            p.span,
                        );
                    }
                }
                self.r.paths.insert(p.span, res);
                for seg in &p.segments {
                    self.walk_generic_args(&seg.args);
                }
            }
            TypeKind::Ref { inner, .. }
            | TypeKind::Ptr { inner, .. }
            | TypeKind::Slice(inner)
            | TypeKind::ChanSend(inner)
            | TypeKind::ChanRecv(inner) => self.walk_type(inner),
            TypeKind::Array(inner, len) => {
                self.walk_type(inner);
                self.walk_expr(len);
            }
            TypeKind::Tuple(ts) => ts.iter().for_each(|t| self.walk_type(t)),
            TypeKind::Fn { params, ret, .. } => {
                params.iter().for_each(|t| self.walk_type(t));
                if let Some(r) = ret {
                    self.walk_type(r);
                }
            }
            TypeKind::Dyn(b) | TypeKind::Impl(b) => self.walk_bounds(b),
            TypeKind::Never | TypeKind::Infer => {}
        }
    }

    // ----- path resolution ---------------------------------------------------

    fn lookup_type_name(&self, name: &str) -> Option<Res> {
        if self
            .ty_params
            .iter()
            .rev()
            .any(|ps| ps.iter().any(|p| p == name))
        {
            return Some(Res::TyParam(name.to_string()));
        }
        if name == "Self" && self.self_ty_allowed {
            return Some(Res::SelfTy);
        }
        for s in self.scopes.iter().rev() {
            if let Some(&id) = s.types.get(name) {
                return Some(Res::Def(id));
            }
        }
        if let Some(&id) = self.r.modules[&self.module].types.get(name) {
            return Some(Res::Def(id));
        }
        if let Some(&id) = self.r.modules[&self.r.prelude].types.get(name) {
            return Some(Res::Def(id));
        }
        PrimTy::from_name(name).map(Res::Prim)
    }

    fn lookup_value_name(&self, name: &str, include_locals: bool) -> Option<Res> {
        for s in self.scopes.iter().rev() {
            match s.values.get(name) {
                Some(Entry::Local(id)) if include_locals => return Some(Res::Local(*id)),
                Some(Entry::Def(id)) => return Some(Res::Def(*id)),
                _ => {}
            }
        }
        if let Some(&id) = self.r.modules[&self.module].values.get(name) {
            return Some(Res::Def(id));
        }
        if let Some(&id) = self.r.modules[&self.r.prelude].values.get(name) {
            return Some(Res::Def(id));
        }
        match name {
            "drop" => Some(Res::Builtin(Builtin::Drop)),
            _ => None,
        }
    }

    /// Whether module `target` is `self.module` or one of its ancestors.
    fn can_see_private_of(&self, target: DefId) -> bool {
        let mut m = Some(self.module);
        while let Some(id) = m {
            if id == target {
                return true;
            }
            m = self.r.modules[&id].parent;
        }
        false
    }

    fn resolve_path(&mut self, p: &Path, ns: Ns) -> Res {
        let first = &p.segments[0].ident;
        if p.segments.len() == 1 {
            let found = match ns {
                Ns::Type => self.lookup_type_name(&first.name),
                Ns::Value => {
                    if first.name == "self" {
                        match self.self_local {
                            Some(id) => Some(Res::Local(id)),
                            None => {
                                self.error("`self` is only available in methods", first.span);
                                return Res::Err;
                            }
                        }
                    } else if first.name == "Self" && self.self_ty_allowed {
                        Some(Res::SelfTy)
                    } else if first.name == "chan" {
                        Some(Res::Builtin(Builtin::Chan))
                    } else {
                        self.lookup_value_name(&first.name, true)
                    }
                }
            };
            return match found {
                Some(r) => r,
                None => {
                    let what = if ns == Ns::Type { "type" } else { "value" };
                    let mut d = Diagnostic::error(
                        format!("cannot find {what} `{}` in this scope", first.name),
                        first.span,
                    );
                    if ns == Ns::Value && self.lookup_type_name(&first.name).is_some() {
                        d = d.with_help(format!(
                            "`{}` is a type; use a struct literal or call a constructor",
                            first.name
                        ));
                    } else if ns == Ns::Type && self.lookup_value_name(&first.name, false).is_some()
                    {
                        d = d.with_help(format!("`{}` is a value, not a type", first.name));
                    }
                    self.r.errors.push(d);
                    Res::Err
                }
            };
        }
        // Multi-segment path: resolve the first segment as a type/module.
        let mut cur = if first.name == "self" {
            Res::Def(self.module)
        } else {
            match self.lookup_type_name(&first.name) {
                Some(r) => r,
                None => {
                    self.error(
                        format!("cannot find module or type `{}` in this scope", first.name),
                        first.span,
                    );
                    return Res::Err;
                }
            }
        };
        let n = p.segments.len();
        for (i, seg) in p.segments.iter().enumerate().skip(1) {
            let last = i == n - 1;
            let name = &seg.ident.name;
            cur = match cur {
                Res::Def(id) => match self.r.def(id).kind {
                    DefKind::Mod => {
                        let scope = &self.r.modules[&id];
                        let (a, b) = (
                            scope.types.get(name).copied(),
                            scope.values.get(name).copied(),
                        );
                        let found = if !last || ns == Ns::Type {
                            a.or(b)
                        } else {
                            b.or(a)
                        };
                        let is_type = !last || ns == Ns::Type;
                        let found_is_type = if is_type { a.is_some() } else { b.is_none() };
                        if found.is_some()
                            && self.hidden_import(id, found_is_type, name, self.module)
                        {
                            self.error_help(
                                format!(
                                    "`{name}` is imported privately in module `{}`",
                                    self.r.def(id).name
                                ),
                                seg.ident.span,
                                format!("import it from where it is defined, or make it `pub import` in `{}`", self.r.def(id).name),
                            );
                            return Res::Err;
                        }
                        match found {
                            Some(d) => {
                                if self.r.def(d).vis == Visibility::Private
                                    && !self.can_see_private_of(id)
                                {
                                    self.error(format!("`{name}` is private"), seg.ident.span);
                                }
                                Res::Def(d)
                            }
                            None => {
                                self.error(
                                    format!(
                                        "cannot find `{name}` in module `{}`",
                                        self.r.def(id).name
                                    ),
                                    seg.ident.span,
                                );
                                return Res::Err;
                            }
                        }
                    }
                    DefKind::Enum => match self
                        .r
                        .variants(id)
                        .into_iter()
                        .find(|v| &self.r.def(*v).name == name)
                    {
                        Some(v) if last => Res::Def(v),
                        Some(_) => {
                            self.error("enum variants have no associated items", seg.ident.span);
                            return Res::Err;
                        }
                        // `Enum::function`: an associated function from an `impl`.
                        None if last => Res::Assoc {
                            base: Box::new(Res::Def(id)),
                            name: name.clone(),
                        },
                        None => {
                            self.error(
                                format!(
                                    "no variant named `{name}` in enum `{}`",
                                    self.r.def(id).name
                                ),
                                seg.ident.span,
                            );
                            return Res::Err;
                        }
                    },
                    DefKind::External => return Res::External,
                    DefKind::Struct | DefKind::TypeAlias | DefKind::Trait if last => Res::Assoc {
                        base: Box::new(Res::Def(id)),
                        name: name.clone(),
                    },
                    k => {
                        self.error(
                            format!(
                                "expected module or type, found {} `{}`",
                                kind_name(k),
                                self.r.def(id).name
                            ),
                            seg.ident.span,
                        );
                        return Res::Err;
                    }
                },
                r @ (Res::Prim(_) | Res::TyParam(_) | Res::SelfTy) if last => Res::Assoc {
                    base: Box::new(r),
                    name: name.clone(),
                },
                Res::External => return Res::External,
                _ => {
                    self.error(format!("cannot resolve `{name}` here"), seg.ident.span);
                    return Res::Err;
                }
            };
        }
        cur
    }

    // ----- bodies ----------------------------------------------------------------

    fn walk_block(&mut self, b: &'a Block) {
        self.scopes.push(Scope::default());
        // Nested items are visible throughout the block.
        let items: Vec<&'a Item> = b
            .stmts
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Item(i) => Some(&**i),
                _ => None,
            })
            .collect();
        if !items.is_empty() {
            for item in &items {
                for (name, ns, id) in self.define_items(std::slice::from_ref(*item), self.module) {
                    let scope = self.scopes.last_mut().unwrap();
                    match ns {
                        Ns::Type => {
                            scope.types.insert(name.name.clone(), id);
                        }
                        Ns::Value => {
                            scope.values.insert(name.name.clone(), Entry::Def(id));
                        }
                    }
                }
            }
        }
        for s in &b.stmts {
            self.walk_stmt(s);
        }
        self.scopes.pop();
    }

    fn walk_stmt(&mut self, s: &'a Stmt) {
        match &s.kind {
            StmtKind::Let { pat, ty, init } => {
                if let Some(e) = init {
                    self.walk_expr(e);
                }
                if let Some(t) = ty {
                    self.walk_type(t);
                }
                self.bind_pattern(pat, &mut HashSet::new());
            }
            StmtKind::ShortDecl { names, init, .. } => {
                self.walk_expr(init);
                let mut seen = HashSet::new();
                for n in names {
                    if let Some(id) = &n.name {
                        if !seen.insert(id.name.clone()) {
                            self.error(
                                format!(
                                    "identifier `{}` is bound more than once in this declaration",
                                    id.name
                                ),
                                id.span,
                            );
                        }
                        self.declare(id, n.mutable);
                    }
                }
            }
            StmtKind::Assign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Send { chan, value } => {
                self.walk_expr(chan);
                self.walk_expr(value);
            }
            StmtKind::Defer(e) | StmtKind::Expr(e) => self.walk_expr(e),
            StmtKind::Item(item) => self.walk_item(item),
        }
    }

    /// Binds the names in a pattern. Single identifiers that name a unit
    /// variant, unit struct, or constant are treated as paths, not bindings.
    fn bind_pattern(&mut self, p: &Pattern, seen: &mut HashSet<String>) {
        match &p.kind {
            PatternKind::Ident { name, mutable, sub } => {
                if !*mutable && sub.is_none() {
                    if let Some(Res::Def(id)) = self.lookup_value_name(&name.name, false) {
                        let def = self.r.def(id);
                        let is_unit = match def.kind {
                            DefKind::Variant { .. } => {
                                matches!(def.variant.map(|v| &v.fields), Some(Fields::Unit))
                            }
                            DefKind::Struct => matches!(
                                def.item.map(|i| &i.kind),
                                Some(ItemKind::Struct(StructDecl {
                                    fields: Fields::Unit,
                                    ..
                                }))
                            ),
                            DefKind::Const | DefKind::Static => true,
                            _ => false,
                        };
                        if is_unit {
                            self.r.paths.insert(name.span, Res::Def(id));
                            return;
                        }
                    }
                }
                if !seen.insert(name.name.clone()) {
                    self.error(
                        format!(
                            "identifier `{}` is bound more than once in the same pattern",
                            name.name
                        ),
                        name.span,
                    );
                }
                self.declare(name, *mutable);
                if let Some(s) = sub {
                    self.bind_pattern(s, seen);
                }
            }
            PatternKind::Wild
            | PatternKind::Rest
            | PatternKind::Lit(_)
            | PatternKind::Range { .. } => {}
            PatternKind::Tuple(ps) | PatternKind::Slice(ps) => {
                ps.iter().for_each(|p| self.bind_pattern(p, seen))
            }
            PatternKind::Path(path) => {
                let r = self.resolve_path(path, Ns::Value);
                self.r.paths.insert(path.span, r);
            }
            PatternKind::TupleStruct(path, ps) => {
                let r = self.resolve_path(path, Ns::Value);
                self.r.paths.insert(path.span, r);
                ps.iter().for_each(|p| self.bind_pattern(p, seen));
            }
            PatternKind::Struct { path, fields, .. } => {
                let r = self.resolve_path(path, Ns::Type);
                self.r.paths.insert(path.span, r);
                for f in fields {
                    self.bind_pattern(&f.pat, seen);
                }
            }
            PatternKind::Ref { inner, .. } => self.bind_pattern(inner, seen),
            PatternKind::Or(alts) => {
                // The first alternative declares the bindings; the others must
                // bind the same names, which map to the same locals.
                self.bind_pattern(&alts[0], seen);
                let first_names = pattern_names(&alts[0]);
                for alt in &alts[1..] {
                    let names = pattern_names(alt);
                    let a: HashSet<_> = first_names.iter().map(|i| &i.name).collect();
                    let b: HashSet<_> = names.iter().map(|i| &i.name).collect();
                    if a != b {
                        self.error(
                            "all alternatives of an `|` pattern must bind the same names",
                            alt.span,
                        );
                    }
                    for n in &names {
                        if let Some(Entry::Local(id)) =
                            self.scopes.last().unwrap().values.get(&n.name).copied()
                        {
                            self.r.bindings.insert(n.span, id);
                        }
                    }
                    self.walk_pattern_paths(alt);
                }
            }
        }
    }

    /// Resolves paths inside a pattern without creating bindings.
    fn walk_pattern_paths(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Path(path) | PatternKind::TupleStruct(path, _) => {
                let r = self.resolve_path(path, Ns::Value);
                self.r.paths.insert(path.span, r);
            }
            PatternKind::Struct { path, .. } => {
                let r = self.resolve_path(path, Ns::Type);
                self.r.paths.insert(path.span, r);
            }
            _ => {}
        }
        match &p.kind {
            PatternKind::Tuple(ps)
            | PatternKind::Slice(ps)
            | PatternKind::TupleStruct(_, ps)
            | PatternKind::Or(ps) => ps.iter().for_each(|p| self.walk_pattern_paths(p)),
            PatternKind::Struct { fields, .. } => {
                fields.iter().for_each(|f| self.walk_pattern_paths(&f.pat))
            }
            PatternKind::Ref { inner, .. } => self.walk_pattern_paths(inner),
            PatternKind::Ident { sub: Some(s), .. } => self.walk_pattern_paths(s),
            _ => {}
        }
    }

    fn walk_scoped<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.scopes.push(Scope::default());
        let r = f(self);
        self.scopes.pop();
        r
    }

    fn check_label(&mut self, label: &Option<Ident>, what: &str, span: Span) {
        if self.loops.is_empty() {
            self.error(format!("`{what}` outside of a loop"), span);
            return;
        }
        if let Some(l) = label {
            if !self
                .loops
                .iter()
                .any(|x| x.as_deref() == Some(l.name.as_str()))
            {
                self.error(format!("use of undeclared label `'{}`", l.name), l.span);
            }
        }
    }

    fn walk_loop_body(&mut self, label: &Option<Ident>, body: &'a Block) {
        self.loops.push(label.as_ref().map(|l| l.name.clone()));
        self.walk_block(body);
        self.loops.pop();
    }

    fn walk_expr(&mut self, e: &'a Expr) {
        match &e.kind {
            ExprKind::Lit(_) | ExprKind::Error => {}
            ExprKind::Interp(parts) => {
                for p in parts {
                    if let InterpPart::Expr(x, _) = p {
                        self.walk_expr(x);
                    }
                }
            }
            ExprKind::Path(p) => {
                let r = self.resolve_path(p, Ns::Value);
                if let Res::Def(id) = r {
                    let k = self.r.def(id).kind;
                    if matches!(
                        k,
                        DefKind::Trait | DefKind::Mod | DefKind::TypeAlias | DefKind::Enum
                    ) || (k == DefKind::Struct
                        && matches!(
                            self.r.def(id).item.map(|i| &i.kind),
                            Some(ItemKind::Struct(StructDecl {
                                fields: Fields::Named(_),
                                ..
                            }))
                        ))
                    {
                        self.error(
                            format!(
                                "expected value, found {} `{}`",
                                kind_name(k),
                                self.r.def(id).name
                            ),
                            p.span,
                        );
                    }
                }
                self.r.paths.insert(p.span, r);
                for seg in &p.segments {
                    self.walk_generic_args(&seg.args);
                }
            }
            ExprKind::Unary(_, x)
            | ExprKind::Try(x)
            | ExprKind::Paren(x)
            | ExprKind::Field(x, _)
            | ExprKind::TupleIndex(x, _) => self.walk_expr(x),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::ArrayRepeat(a, b) => {
                self.walk_expr(a);
                self.walk_expr(b);
            }
            ExprKind::Cast(x, t) => {
                self.walk_expr(x);
                self.walk_type(t);
            }
            ExprKind::Call(f, args) => {
                self.walk_expr(f);
                args.iter().for_each(|a| self.walk_expr(a));
            }
            ExprKind::MethodCall {
                receiver,
                generic_args,
                args,
                ..
            } => {
                self.walk_expr(receiver);
                self.walk_generic_args(generic_args);
                args.iter().for_each(|a| self.walk_expr(a));
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(s) = end {
                    self.walk_expr(s);
                }
            }
            ExprKind::StructLit { path, fields, base } => {
                let r = self.resolve_path(path, Ns::Type);
                self.r.paths.insert(path.span, r);
                for f in fields {
                    self.walk_expr(&f.value);
                }
                if let Some(b) = base {
                    self.walk_expr(b);
                }
            }
            ExprKind::Tuple(xs) | ExprKind::Array(xs) => xs.iter().for_each(|x| self.walk_expr(x)),
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.walk_block(b),
            ExprKind::Go(g) => match &**g {
                GoTarget::Call(e) => self.walk_expr(e),
                GoTarget::Block { body, .. } => {
                    let saved = std::mem::take(&mut self.loops);
                    self.walk_block(body);
                    self.loops = saved;
                }
            },
            ExprKind::If { cond, then, els } => {
                self.walk_expr(cond);
                self.walk_block(then);
                if let Some(e) = els {
                    self.walk_expr(e);
                }
            }
            ExprKind::IfLet {
                pat,
                expr,
                then,
                els,
            } => {
                self.walk_expr(expr);
                self.walk_scoped(|s| {
                    s.bind_pattern(pat, &mut HashSet::new());
                    s.walk_block(then);
                });
                if let Some(e) = els {
                    self.walk_expr(e);
                }
            }
            ExprKind::Match(scrut, arms) => {
                self.walk_expr(scrut);
                for arm in arms {
                    self.walk_scoped(|s| {
                        s.bind_pattern(&arm.pat, &mut HashSet::new());
                        if let Some(g) = &arm.guard {
                            s.walk_expr(g);
                        }
                        s.walk_expr(&arm.body);
                    });
                }
            }
            ExprKind::Select(arms) => {
                for arm in arms {
                    self.walk_scoped(|s| {
                        match &arm.op {
                            SelectOp::Recv { bind, chan } => {
                                s.walk_expr(chan);
                                if let Some(ShortName {
                                    name: Some(n),
                                    mutable,
                                }) = bind
                                {
                                    s.declare(n, *mutable);
                                }
                            }
                            SelectOp::Send { chan, value } => {
                                s.walk_expr(chan);
                                s.walk_expr(value);
                            }
                            SelectOp::Default => {}
                        }
                        s.walk_expr(&arm.body);
                    });
                }
            }
            ExprKind::For {
                label,
                pat,
                iter,
                body,
            } => {
                self.walk_expr(iter);
                self.walk_scoped(|s| {
                    s.bind_pattern(pat, &mut HashSet::new());
                    s.walk_loop_body(label, body);
                });
            }
            ExprKind::While { label, cond, body } => {
                self.walk_expr(cond);
                self.walk_loop_body(label, body);
            }
            ExprKind::WhileLet {
                label,
                pat,
                expr,
                body,
            } => {
                self.walk_expr(expr);
                self.walk_scoped(|s| {
                    s.bind_pattern(pat, &mut HashSet::new());
                    s.walk_loop_body(label, body);
                });
            }
            ExprKind::Loop { label, body } => self.walk_loop_body(label, body),
            ExprKind::Break(label, value) => {
                self.check_label(label, "break", e.span);
                if let Some(v) = value {
                    self.walk_expr(v);
                }
            }
            ExprKind::Continue(label) => self.check_label(label, "continue", e.span),
            ExprKind::Return(v) => {
                if let Some(v) = v {
                    self.walk_expr(v);
                }
            }
            ExprKind::Closure {
                params, ret, body, ..
            } => {
                for p in params {
                    if let Some(t) = &p.ty {
                        self.walk_type(t);
                    }
                }
                if let Some(t) = ret {
                    self.walk_type(t);
                }
                let saved = std::mem::take(&mut self.loops);
                self.walk_scoped(|s| {
                    let mut seen = HashSet::new();
                    for p in params {
                        s.bind_pattern(&p.pat, &mut seen);
                    }
                    s.walk_expr(body);
                });
                self.loops = saved;
            }
        }
    }
}

/// All identifiers bound by a pattern.
fn pattern_names(p: &Pattern) -> Vec<Ident> {
    let mut out = Vec::new();
    fn go(p: &Pattern, out: &mut Vec<Ident>) {
        match &p.kind {
            PatternKind::Ident { name, sub, .. } => {
                out.push(name.clone());
                if let Some(s) = sub {
                    go(s, out);
                }
            }
            PatternKind::Tuple(ps) | PatternKind::Slice(ps) | PatternKind::TupleStruct(_, ps) => {
                ps.iter().for_each(|p| go(p, out))
            }
            PatternKind::Struct { fields, .. } => fields.iter().for_each(|f| go(&f.pat, out)),
            PatternKind::Ref { inner, .. } => go(inner, out),
            PatternKind::Or(alts) => go(&alts[0], out),
            _ => {}
        }
    }
    go(p, &mut out);
    out
}

/// Human-readable name of a definition kind.
pub fn kind_name(k: DefKind) -> &'static str {
    match k {
        DefKind::Mod => "module",
        DefKind::Struct => "struct",
        DefKind::Enum => "enum",
        DefKind::Variant { .. } => "variant",
        DefKind::Trait => "trait",
        DefKind::Fn => "function",
        DefKind::Const => "constant",
        DefKind::Static => "static",
        DefKind::TypeAlias => "type alias",
        DefKind::External => "external item",
    }
}

/// `ty` with every elided reference lifetime made `'static` (references in a
/// static's type live forever).
pub fn static_lifetimes(ty: &Type) -> Type {
    let mut t = ty.clone();
    fn fix(t: &mut Type) {
        match &mut t.kind {
            TypeKind::Ref {
                lifetime, inner, ..
            } => {
                if lifetime.is_none() {
                    *lifetime = Some(Ident {
                        name: "static".into(),
                        span: t.span,
                    });
                }
                fix(inner);
            }
            TypeKind::Ptr { inner, .. } | TypeKind::Slice(inner) | TypeKind::Array(inner, _) => {
                fix(inner)
            }
            TypeKind::Tuple(ts) => ts.iter_mut().for_each(fix),
            TypeKind::Path(p) => {
                for seg in &mut p.segments {
                    for a in &mut seg.args {
                        if let GenericArg::Type(t) = a {
                            fix(t);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fix(&mut t);
    t
}

/// A hidden function `fn name() -> ty { return value }` for a constant.
fn const_fn(name: String, ident: &Ident, ty: &Type, value: &Expr) -> &'static FnDecl {
    let ret = Expr {
        kind: ExprKind::Return(Some(Box::new(value.clone()))),
        span: ident.span,
    };
    Box::leak(Box::new(FnDecl {
        name: Ident {
            name,
            span: ident.span,
        },
        is_unsafe: false,
        generics: Generics::default(),
        self_param: None,
        params: vec![],
        ret: Some(static_lifetimes(ty)),
        body: Some(Block {
            stmts: vec![Stmt {
                kind: StmtKind::Expr(ret),
                span: ident.span,
            }],
            span: value.span,
        }),
    }))
}
