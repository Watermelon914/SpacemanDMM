//! The object tree representation, used as a parsing target.

use std::collections::BTreeMap;
use std::fmt;

pub use petgraph::graph::NodeIndex;
use petgraph::graph::Graph;
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use linked_hash_map::LinkedHashMap;

use super::ast::{Expression, VarType, VarSuffix, PathOp, Parameter, Statement};
use super::constants::{Constant, Pop};
use super::docs::DocCollection;
use super::{DMError, Location, Context};

// ----------------------------------------------------------------------------
// Variables

pub type Vars = LinkedHashMap<String, Constant>;

#[derive(Debug, Clone)]
pub struct VarDeclaration {
    pub var_type: VarType,
    pub location: Location,
}

#[derive(Debug, Clone)]
pub struct VarValue {
    pub location: Location,
    /// Syntactic value, as specified in the source.
    pub expression: Option<Expression>,
    /// Evaluated value for non-static and non-tmp vars.
    pub constant: Option<Constant>,
    pub being_evaluated: bool,
    pub docs: DocCollection,
}

#[derive(Debug, Clone)]
pub struct TypeVar {
    pub value: VarValue,
    pub declaration: Option<VarDeclaration>,
}

#[derive(Debug, Clone)]
pub struct ProcDeclaration {
    pub location: Location,
    pub is_verb: bool,
}

#[derive(Debug, Clone)]
pub struct ProcValue {
    pub location: Location,
    pub parameters: Vec<Parameter>,
    pub docs: DocCollection,
    pub code: Code,
}

#[derive(Debug, Clone)]
pub enum Code {
    Present(Vec<Statement>),
    Invalid(DMError),
    Builtin,
    Disabled,
}

#[derive(Debug, Clone, Default)]
pub struct TypeProc {
    pub value: Vec<ProcValue>,
    pub declaration: Option<ProcDeclaration>,
}

// ----------------------------------------------------------------------------
// Types

const BAD_NODE_INDEX: usize = ::std::usize::MAX;

#[derive(Debug)]
pub struct Type {
    pub name: String,
    pub path: String,
    pub location: Location,
    location_specificity: usize,
    pub vars: LinkedHashMap<String, TypeVar>,
    pub procs: LinkedHashMap<String, TypeProc>,
    parent_type: NodeIndex,
    pub docs: DocCollection,
}

impl Type {
    pub fn parent_type(&self) -> Option<NodeIndex> {
        if self.parent_type == NodeIndex::new(BAD_NODE_INDEX) {
            None
        } else {
            Some(self.parent_type)
        }
    }

    /// Checks whether this node is the root node, on which global vars and
    /// procs reside.
    #[inline]
    pub fn is_root(&self) -> bool {
        self.path.is_empty()
    }

    pub fn pretty_path(&self) -> &str {
        if self.is_root() {
            "(global)"
        } else {
            &self.path
        }
    }

    /// Checks whether this type's path is a subpath of the given path.
    #[inline]
    pub fn is_subpath_of(&self, parent: &str) -> bool {
        subpath(&self.path, parent)
    }

    // Used in the constant evaluator which holds an &mut ObjectTree and thus
    // can't be used with TypeRef.
    pub(crate) fn get_value<'a>(&'a self, name: &str, objtree: &'a ObjectTree) -> Option<&'a VarValue> {
        let mut current = Some(self);
        while let Some(ty) = current {
            if let Some(var) = ty.vars.get(name) {
                return Some(&var.value);
            }
            current = objtree.parent_of(ty);
        }
        None
    }

    pub(crate) fn get_var_declaration<'a>(&'a self, name: &str, objtree: &'a ObjectTree) -> Option<&'a VarDeclaration> {
        let mut current = Some(self);
        while let Some(ty) = current {
            if let Some(var) = ty.vars.get(name) {
                if let Some(ref decl) = var.declaration {
                    return Some(decl);
                }
            }
            current = objtree.parent_of(ty);
        }
        None
    }
}

#[inline]
pub fn subpath(path: &str, parent: &str) -> bool {
    debug_assert!(path.starts_with('/') && parent.starts_with('/') && parent.ends_with('/'));
    path == &parent[..parent.len() - 1] || path.starts_with(parent)
}

// ----------------------------------------------------------------------------
// Type references

#[derive(Copy, Clone)]
pub struct TypeRef<'a> {
    tree: &'a ObjectTree,
    idx: NodeIndex,
}

impl<'a> TypeRef<'a> {
    #[inline]
    pub(crate) fn new(tree: &'a ObjectTree, idx: NodeIndex) -> TypeRef<'a> {
        TypeRef { tree, idx }
    }

    #[inline]
    pub fn get(self) -> &'a Type {
        self.tree.graph.node_weight(self.idx).unwrap()
    }

    /// Find the parent **path**, without taking `parent_type` into account.
    pub fn parent_path(&self) -> Option<TypeRef<'a>> {
        self.tree
            .graph
            .neighbors_directed(self.idx, Direction::Incoming)
            .next()
            .map(|i| TypeRef::new(self.tree, i))
    }

    /// Find the parent **type** based on `parent_type` var, or parent path if unspecified.
    pub fn parent_type(&self) -> Option<TypeRef<'a>> {
        let idx = self.parent_type;
        self.tree.graph.node_weight(idx).map(|_| TypeRef::new(self.tree, idx))
    }

    /// Find a child **path** with the given name, if it exists.
    pub fn child(&self, name: &str) -> Option<TypeRef<'a>> {
        for idx in self.tree.graph.neighbors(self.idx) {
            let ty = self.tree.graph.node_weight(idx).unwrap();
            if ty.name == name {
                return Some(TypeRef::new(self.tree, idx));
            }
        }
        None
    }

    /// Iterate over all child **paths**.
    pub fn children(&self) -> Vec<TypeRef<'a>> {
        let mut output = Vec::new();
        for idx in self.tree.graph.neighbors(self.idx) {
            output.push(TypeRef::new(self.tree, idx));
        }
        output
    }

    /// Recursively visit this and all child **paths**.
    pub fn recurse<F: FnMut(TypeRef<'a>)>(&self, f: &mut F) {
        f(*self);
        for child in self.children() {
            child.recurse(f);
        }
    }

    /// Recursively visit this and all parent **types**.
    pub fn visit_parent_types<F: FnMut(TypeRef<'a>)>(&self, f: &mut F) {
        let mut next = Some(*self);
        while let Some(current) = next {
            f(current);
            next = current.parent_type();
        }
    }

    /// Recursively visit this and all parent **paths**.
    pub fn visit_parent_paths<F: FnMut(TypeRef<'a>)>(&self, f: &mut F) {
        let mut next = Some(*self);
        while let Some(current) = next {
            f(current);
            next = current.parent_path();
        }
    }

    /// Navigate the tree according to the given path operator.
    pub fn navigate(self, op: PathOp, name: &str) -> Option<TypeRef<'a>> {
        match op {
            // '/' always looks for a direct child
            PathOp::Slash => self.child(name),
            // '.' looks for a child of us or of any of our parents
            PathOp::Dot => {
                let mut next = Some(self);
                while let Some(current) = next {
                    if let Some(child) = current.child(name) {
                        return Some(child);
                    }
                    next = current.parent_path();
                }
                None
            },
            // ':' looks for a child of us or of any of our children
            PathOp::Colon => {
                if let Some(child) = self.child(name) {
                    return Some(child);
                }
                for idx in self.tree.graph.neighbors(self.idx) {
                    if let Some(child) = TypeRef::new(self.tree, idx).navigate(PathOp::Colon, name) {
                        // Yes, simply returning the first thing that matches
                        // is the correct behavior.
                        return Some(child);
                    }
                }
                None
            },
        }
    }

    /// Find another type relative to this type.
    pub fn navigate_path<S: AsRef<str>>(self, pieces: &[(PathOp, S)]) -> Option<TypeRef<'a>> {
        let mut iter = pieces.iter();
        let mut next = match iter.next() {
            Some(&(PathOp::Slash, ref s)) => self.tree.root().child(s.as_ref()),
            Some(&(op, ref s)) => self.navigate(op, s.as_ref()),
            None => return Some(self),
        };
        for &(op, ref s) in iter {
            if let Some(current) = next {
                next = current.navigate(op, s.as_ref());
            } else {
                return None;
            }
        }
        next
    }

    /// Checks whether this type is a subtype of the given type.
    pub fn is_subtype_of(self, parent: &Type) -> bool {
        let mut current = Some(self);
        while let Some(ty) = current.take() {
            if ::std::ptr::eq(ty.get(), parent) {
                return true;
            }
            current = ty.parent_type();
        }
        false
    }

    #[inline]
    pub fn get_value(self, name: &str) -> Option<&'a VarValue> {
        self.get().get_value(name, self.tree)
    }

    #[inline]
    pub fn get_var_declaration(self, name: &str) -> Option<&'a VarDeclaration> {
        self.get().get_var_declaration(name, self.tree)
    }

    pub fn get_proc(self, name: &'a str) -> Option<ProcRef<'a>> {
        let mut current: Option<TypeRef<'a>> = Some(self);
        while let Some(ty) = current {
            if let Some(proc) = ty.get().procs.get(name) {
                return Some(ProcRef {
                    ty,
                    list: &proc.value,
                    name,
                    idx: proc.value.len() - 1,
                });
            }
            current = ty.parent_type();
        }
        None
    }

    pub fn get_proc_declaration(self, name: &str) -> Option<&'a ProcDeclaration> {
        let mut current: Option<TypeRef<'a>> = Some(self);
        while let Some(ty) = current {
            if let Some(proc) = ty.get().procs.get(name) {
                if let Some(decl) = proc.declaration.as_ref() {
                    return Some(decl);
                }
            }
            current = ty.parent_type();
        }
        None
    }

    pub fn iter_self_procs(self) -> impl Iterator<Item=ProcRef<'a>> {
        self.get().procs.iter().flat_map(move |(name, type_proc)| {
            let list = &type_proc.value;
            (0..list.len()).map(move |idx| ProcRef {
                ty: self,
                list,
                name,
                idx,
            })
        })
    }
}

impl<'a> ::std::ops::Deref for TypeRef<'a> {
    type Target = Type;
    fn deref(&self) -> &Type {
        self.get()
    }
}

impl<'a> fmt::Debug for TypeRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}({})", self.path, self.idx.index())
    }
}

impl<'a> fmt::Display for TypeRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.pretty_path())
    }
}

impl<'a> ::std::cmp::PartialEq for TypeRef<'a> {
    fn eq(&self, other: &Self) -> bool {
        ::std::ptr::eq(self.tree, other.tree) && self.idx == other.idx
    }
}

impl<'a> ::std::cmp::Eq for TypeRef<'a> {}

// ----------------------------------------------------------------------------
// Proc references

#[derive(Clone, Copy)]
pub struct ProcRef<'a> {
    ty: TypeRef<'a>,
    list: &'a [ProcValue],
    name: &'a str,
    idx: usize,
}

impl<'a> ProcRef<'a> {
    pub fn get(self) -> &'a ProcValue {
        &self.list[self.idx]
    }

    pub fn ty(self) -> TypeRef<'a> {
        self.ty
    }

    pub fn name(&self) -> &str {
        self.name
    }

    pub fn index(self) -> usize {
        self.idx
    }

    pub fn parent_proc(self) -> Option<ProcRef<'a>> {
        if let Some(idx) = self.idx.checked_sub(1) {
            Some(ProcRef {
                ty: self.ty,
                list: self.list,
                name: self.name,
                idx,
            })
        } else {
            self.ty.parent_type().and_then(|ty| ty.get_proc(self.name))
        }
    }
}

impl<'a> ::std::ops::Deref for ProcRef<'a> {
    type Target = ProcValue;
    fn deref(&self) -> &ProcValue {
        self.get()
    }
}

impl<'a> fmt::Debug for ProcRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}/proc/{}[{}/{}]", self.ty, self.name, self.idx, self.list.len())
    }
}

impl<'a> fmt::Display for ProcRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/proc/{}", self.ty.path, self.name)?;
        if self.list.len() > 1 {
            write!(f, "[{}/{}]", self.idx, self.list.len())?;
        }
        Ok(())
    }
}

impl<'a> ::std::cmp::PartialEq for ProcRef<'a> {
    fn eq(&self, other: &ProcRef<'a>) -> bool {
        self.ty == other.ty && self.name == other.name && self.idx == other.idx
    }
}

impl<'a> std::cmp::Eq for ProcRef<'a> {}

// ----------------------------------------------------------------------------
// The object tree itself

#[derive(Debug)]
pub struct ObjectTree {
    pub graph: Graph<Type, ()>,
    pub types: BTreeMap<String, NodeIndex>,
}

impl Default for ObjectTree {
    fn default() -> Self {
        let mut tree = ObjectTree {
            graph: Default::default(),
            types: Default::default(),
        };
        tree.graph.add_node(Type {
            name: String::new(),
            path: String::new(),
            location: Default::default(),
            location_specificity: 0,
            vars: Default::default(),
            procs: Default::default(),
            parent_type: NodeIndex::new(BAD_NODE_INDEX),
            docs: Default::default(),
        });
        tree
    }
}

impl ObjectTree {
    pub fn register_builtins(&mut self) {
        super::builtins::register_builtins(self).expect("register_builtins failed");
    }

    // ------------------------------------------------------------------------
    // Access

    pub fn root(&self) -> TypeRef {
        TypeRef::new(self, NodeIndex::new(0))
    }

    pub fn find(&self, path: &str) -> Option<TypeRef> {
        self.types.get(path).map(|&ix| TypeRef::new(self, ix))
    }

    pub fn expect(&self, path: &str) -> TypeRef {
        match self.types.get(path) {
            Some(&ix) => TypeRef::new(self, ix),
            None => panic!("type not found: {:?}", path),
        }
    }

    pub fn parent_of(&self, type_: &Type) -> Option<&Type> {
        self.graph.node_weight(type_.parent_type)
    }

    pub fn type_by_path<I>(&self, path: I) -> Option<TypeRef>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let (exact, ty) = self.type_by_path_approx(path);
        if exact {
            Some(ty)
        } else {
            None
        }
    }

    pub fn type_by_path_approx<I>(&self, path: I) -> (bool, TypeRef)
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let mut current = NodeIndex::new(0);
        let mut first = true;
        'outer: for each in path {
            let each = each.as_ref();

            for edge in self.graph.edges(current) {
                let target = edge.target();
                if self.graph.node_weight(target).unwrap().name == each {
                    current = target;
                    if each == "list" && first {
                        // any lookup under list/ is list/
                        break 'outer;
                    }
                    first = false;
                    continue 'outer;
                }
            }
            return (false, TypeRef::new(self, current));
    }
        (true, TypeRef::new(self, current))
    }

    pub fn type_by_constant(&self, constant: &Constant) -> Option<TypeRef> {
        match *constant {
            Constant::String(ref string_path) => self.find(string_path),
            Constant::Prefab(Pop { ref path, .. }) => self.type_by_path(path),
            _ => None,
        }
    }

    // ------------------------------------------------------------------------
    // Finalization

    pub(crate) fn finalize(&mut self, context: &Context, sloppy: bool) {
        self.assign_parent_types(context);
        super::constants::evaluate_all(context, self, sloppy);
    }

    fn assign_parent_types(&mut self, context: &Context) {
        for (path, &type_idx) in self.types.iter() {
            let mut location = self.graph.node_weight(type_idx).unwrap().location;
            let idx = if path == "/datum" {
                NodeIndex::new(0)
            } else {
                let mut parent_type_buf;
                let parent_type = if path == "/atom" {
                    "/datum"
                } else if path == "/turf" || path == "/area" {
                    "/atom"
                } else if path == "/obj" || path == "/mob" {
                    "/atom/movable"
                } else {
                    let mut parent_type = match path.rfind('/').unwrap() {
                        0 => "/datum",
                        idx => &path[..idx],
                    };
                    if let Some(name) = self.graph.node_weight(type_idx).unwrap().vars.get("parent_type") {
                        location = name.value.location;
                        if let Some(expr) = name.value.expression.clone() {
                            match expr.simple_evaluate(name.value.location) {
                                Ok(Constant::String(s)) => {
                                    parent_type_buf = s;
                                    parent_type = &parent_type_buf;
                                }
                                Ok(Constant::Prefab(Pop { ref path, ref vars })) if vars.is_empty() => {
                                    parent_type_buf = String::new();
                                    for piece in path.iter() {
                                        parent_type_buf.push('/');
                                        parent_type_buf.push_str(&piece);
                                    }
                                    parent_type = &parent_type_buf;
                                }
                                Ok(other) => {
                                    context.register_error(DMError::new(location, format!("bad parent_type: {}", other)));
                                }
                                Err(e) => {
                                    context.register_error(e);
                                }
                            }
                        }
                    }
                    parent_type
                };

                if let Some(&idx) = self.types.get(parent_type) {
                    idx
                } else {
                    context.register_error(DMError::new(
                        location,
                        format!("bad parent type for {}: {}", path, parent_type),
                    ));
                    NodeIndex::new(0)  // on bad parent_type, fall back to the root
                }
            };

            self.graph.node_weight_mut(type_idx)
                .unwrap()
                .parent_type = idx;
        }
    }

    // ------------------------------------------------------------------------
    // Parsing

    fn subtype_or_add(&mut self, location: Location, parent: NodeIndex, child: &str, len: usize) -> NodeIndex {
        let mut neighbors = self.graph.neighbors(parent).detach();
        while let Some(target) = neighbors.next_node(&self.graph) {
            let node = self.graph.node_weight_mut(target).unwrap();
            if node.name == child {
                if node.location_specificity > len {
                    node.location_specificity = len;
                    node.location = location;
                }
                return target;
            }
        }

        // time to add a new child
        let path = format!("{}/{}", self.graph.node_weight(parent).unwrap().path, child);
        let node = self.graph.add_node(Type {
            name: child.to_owned(),
            path: path.clone(),
            vars: Default::default(),
            procs: Default::default(),
            location,
            location_specificity: len,
            parent_type: NodeIndex::new(BAD_NODE_INDEX),
            docs: Default::default(),
        });
        self.graph.add_edge(parent, node, ());
        self.types.insert(path, node);
        node
    }

    fn get_from_path<'a, I: Iterator<Item=&'a str>>(
        &mut self,
        location: Location,
        mut path: I,
        len: usize,
    ) -> Result<(NodeIndex, &'a str), DMError> {
        let mut current = NodeIndex::new(0);
        let mut last = match path.next() {
            Some(name) => name,
            None => return Err(DMError::new(location, "cannot register root path")),
        };
        if is_decl(last) {
            return Ok((current, last));
        }
        for each in path {
            current = self.subtype_or_add(location, current, last, len);
            last = each;
            if is_decl(last) {
                break;
            }
        }

        Ok((current, last))
    }

    fn register_var<'a, I>(
        &mut self,
        location: Location,
        parent: NodeIndex,
        mut prev: &'a str,
        mut rest: I,
        comment: DocCollection,
        suffix: VarSuffix,
    ) -> Result<Option<&mut TypeVar>, DMError>
    where
        I: Iterator<Item=&'a str>,
    {
        let (mut is_declaration, mut is_static, mut is_const, mut is_tmp) = (false, false, false, false);

        if is_var_decl(prev) {
            is_declaration = true;
            prev = match rest.next() {
                Some(name) => name,
                None => return Ok(None), // var{} block, children will be real vars
            };
            while prev == "global" || prev == "static" || prev == "tmp" || prev == "const" {
                if let Some(name) = rest.next() {
                    is_static |= prev == "global" || prev == "static";
                    is_const |= prev == "const";
                    is_tmp |= prev == "tmp";
                    prev = name;
                } else {
                    return Ok(None); // var/const{} block, children will be real vars
                }
            }
        } else if is_proc_decl(prev) {
            return Err(DMError::new(location, "proc looks like a var"));
        }

        let mut type_path = Vec::new();
        for each in rest {
            type_path.push(prev.to_owned());
            prev = each;
        }
        let mut var_type = VarType {
            is_static,
            is_const,
            is_tmp,
            type_path,
        };
        var_type.suffix(&suffix);

        let node = self.graph.node_weight_mut(parent).unwrap();
        // TODO: warn and merge docs for repeats
        Ok(Some(node.vars.entry(prev.to_owned()).or_insert_with(|| TypeVar {
            value: VarValue {
                location,
                expression: suffix.into_initializer(),
                constant: None,
                being_evaluated: false,
                docs: comment,
            },
            declaration: if is_declaration {
                Some(VarDeclaration {
                    var_type,
                    location,
                })
            } else {
                None
            },
        })))
    }

    fn register_proc(
        &mut self,
        location: Location,
        parent: NodeIndex,
        name: &str,
        is_verb: Option<bool>,
        parameters: Vec<Parameter>,
        code: Code,
    ) -> Result<(usize, &mut ProcValue), DMError> {
        let node = self.graph.node_weight_mut(parent).unwrap();
        let proc = node.procs.entry(name.to_owned()).or_insert_with(Default::default);
        if proc.declaration.is_none() {
            proc.declaration = is_verb.map(|is_verb| ProcDeclaration {
                location,
                is_verb,
            });
        }

        let len = proc.value.len();
        proc.value.push(ProcValue {
            location,
            parameters,
            docs: Default::default(),
            code
        });
        Ok((len, proc.value.last_mut().unwrap()))
    }

    // an entry which may be anything depending on the path
    pub fn add_entry<'a, I: Iterator<Item = &'a str>>(
        &mut self,
        location: Location,
        mut path: I,
        len: usize,
        comment: DocCollection,
        suffix: VarSuffix,
    ) -> Result<(), DMError> {
        let (parent, child) = self.get_from_path(location, &mut path, len)?;
        if is_var_decl(child) {
            self.register_var(location, parent, "var", path, comment, suffix)?;
        } else if is_proc_decl(child) {
            // proc{} block, children will be procs
        } else {
            let idx = self.subtype_or_add(location, parent, child, len);
            self.graph.node_weight_mut(idx).unwrap().docs.extend(comment);
        }
        Ok(())
    }

    // an entry which is definitely a var because a value is specified
    pub fn add_var<'a, I: Iterator<Item = &'a str>>(
        &mut self,
        location: Location,
        mut path: I,
        len: usize,
        expr: Expression,
        comment: DocCollection,
        suffix: VarSuffix,
    ) -> Result<(), DMError> {
        let (parent, initial) = self.get_from_path(location, &mut path, len)?;
        if let Some(type_var) = self.register_var(location, parent, initial, path, comment, suffix)? {
            type_var.value.location = location;
            type_var.value.expression = Some(expr);
            Ok(())
        } else {
            Err(DMError::new(location, "var must have a name"))
        }
    }

    // an entry which is definitely a proc because an argument list is specified
    pub fn add_proc<'a, I: Iterator<Item = &'a str>>(
        &mut self,
        location: Location,
        mut path: I,
        len: usize,
        parameters: Vec<Parameter>,
        code: Code,
    ) -> Result<(usize, &mut ProcValue), DMError> {
        let (parent, mut proc_name) = self.get_from_path(location, &mut path, len)?;
        let mut is_verb = None;
        if is_proc_decl(proc_name) {
            is_verb = Some(proc_name == "verb");
            proc_name = match path.next() {
                Some(name) => name,
                None => return Err(DMError::new(location, "proc must have a name")),
            };
        } else if is_var_decl(proc_name) {
            return Err(DMError::new(location, "var looks like a proc"));
        }
        if let Some(other) = path.next() {
            return Err(DMError::new(
                location,
                format!("proc name must be a single identifier (spurious {:?})", other),
            ));
        }

        self.register_proc(location, parent, proc_name, is_verb, parameters, code)
    }
}

#[inline]
fn is_var_decl(s: &str) -> bool {
    s == "var"
}

#[inline]
fn is_proc_decl(s: &str) -> bool {
    s == "proc" || s == "verb"
}

#[inline]
fn is_decl(s: &str) -> bool {
    is_var_decl(s) || is_proc_decl(s)
}
