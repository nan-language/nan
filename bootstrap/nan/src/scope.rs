//! A command's scope: what it acts on, and the lines of the workspace it covers.

use std::ops::RangeInclusive;
use std::path::Path;

use crate::syntax::grammar::Rule;
use crate::syntax::items::Item;
use crate::syntax::token::TokenKind;
use crate::syntax::tree::NodeId;
use crate::workspace::{Package, Workspace};

pub enum Scope {
    Workspace,
    Package(usize),
    Module(usize, usize),
    File(String),
    /// The lines of an item or member.
    Lines(String, RangeInclusive<u32>),
}

impl Scope {
    /// The scope an argument names, or the default scope: the package holding `current`, or `.`.
    /// Returns the scope and how a verdict line writes an omitted one.
    pub fn resolve(
        workspace: &Workspace,
        current: &Path,
        written: Option<&str>,
    ) -> Option<(Scope, String)> {
        let holding = workspace
            .packages
            .iter()
            .position(|package| current.starts_with(workspace.root.join(&package.dir)));
        let Some(text) = written else {
            return Some(match holding {
                Some(index) => {
                    let package = &workspace.packages[index];
                    (
                        Scope::Package(index),
                        package.name().unwrap_or(&package.dir).to_owned(),
                    )
                }
                None => (Scope::Workspace, ".".to_owned()),
            });
        };
        let scope = if text == "." {
            Some(Scope::Workspace)
        } else if text.ends_with(".nan") {
            workspace.file(text).map(|_| Scope::File(text.to_owned()))
        } else if let Some((path, line)) = text
            .rsplit_once(':')
            .filter(|(path, _)| path.ends_with(".nan"))
            .and_then(|(path, line)| Some((path, line.parse::<u32>().ok()?)))
        {
            let (_, file) = workspace.file(path)?;
            let item = file.syntax.item_at(line)?;
            Some(Scope::Lines(path.to_owned(), file.syntax.item_lines(item)))
        } else {
            named(workspace, text, None)
                .or_else(|| holding.and_then(|index| named(workspace, text, Some(index))))
        };
        scope.map(|scope| (scope, text.to_owned()))
    }

    /// The package, module, item or member a canonical path names.
    pub fn canonical(workspace: &Workspace, text: &str) -> Option<Scope> {
        named(workspace, text, None)
    }

    /// Whether the scope covers a line of a file, or the file as a whole when `line` is absent.
    pub fn holds(&self, workspace: &Workspace, path: &str, line: Option<u32>) -> bool {
        let file = workspace.file(path);
        match self {
            Scope::Workspace => true,
            Scope::Package(index) => {
                file.is_some_and(|(package, _)| std::ptr::eq(package, &workspace.packages[*index]))
            }
            Scope::Module(index, module) => file.is_some_and(|(package, file)| {
                std::ptr::eq(package, &workspace.packages[*index]) && file.module == Some(*module)
            }),
            Scope::File(scope) => scope == path,
            Scope::Lines(scope, lines) => {
                scope == path && line.is_some_and(|line| lines.contains(&line))
            }
        }
    }

    /// The one package the scope lies in, if it holds no other.
    pub fn package<'a>(&self, workspace: &'a Workspace) -> Option<&'a Package> {
        match self {
            Scope::Workspace => match &workspace.packages[..] {
                [package] => Some(package),
                _ => None,
            },
            Scope::Package(index) | Scope::Module(index, _) => Some(&workspace.packages[*index]),
            Scope::File(path) | Scope::Lines(path, _) => {
                workspace.file(path).map(|(package, _)| package)
            }
        }
    }
}

/// The package, module, item or member a path names: a canonical path, or with `relative`, a path
/// as that package's root module writes it.
fn named(workspace: &Workspace, text: &str, relative: Option<usize>) -> Option<Scope> {
    let mut segments: Vec<&str> = text.split("::").collect();
    let index = match relative {
        Some(index) => index,
        None => {
            let index = workspace
                .packages
                .iter()
                .position(|package| package.name() == Some(segments[0]))?;
            segments.remove(0);
            index
        }
    };
    let package = &workspace.packages[index];
    let root = package
        .modules
        .iter()
        .position(|module| module.dirs.iter().any(|dir| dir.path == package.dir));
    match segments[..] {
        [] => Some(Scope::Package(index)),
        [name] => {
            let module = (0..package.modules.len())
                .find(|&module| package.modules[module].name == name && Some(module) != root);
            match module {
                Some(module) => Some(Scope::Module(index, module)),
                None => member(package, root?, name),
            }
        }
        [module, name] => {
            let module = package
                .modules
                .iter()
                .position(|candidate| candidate.name == module)?;
            member(package, module, name)
        }
        _ => None,
    }
}

/// The lines of the item of `module` named `name`, or of the member `Type.member` it names.
fn member(package: &Package, module: usize, name: &str) -> Option<Scope> {
    let items = || {
        package
            .files
            .iter()
            .filter(|file| file.module == Some(module))
            .flat_map(|file| {
                file.syntax.items.iter().filter_map(move |item| {
                    let tree = item.tree.as_ref()?;
                    Some((
                        file,
                        item,
                        Item {
                            file: &file.syntax,
                            tree,
                        },
                    ))
                })
            })
    };
    let named = |name: &str| items().find(|(_, _, view)| view.name().as_deref() == Some(name));
    if let Some((file, item, _)) = named(name) {
        return Some(Scope::Lines(
            file.path.clone(),
            file.syntax.item_lines(item),
        ));
    }
    let (owner, member) = name.split_once('.')?;
    let (file, _, view) = named(owner)?;
    let (_, node) = view.kind()?;
    let (first, last) = view.tree.span(member_node(view, node, member)?)?;
    let lines = file.syntax.tokens[first as usize].line..=file.syntax.tokens[last as usize].line;
    Some(Scope::Lines(file.path.clone(), lines))
}

/// The member line of a type or interface declaring `name`: a field, variant, tag, register or
/// method.
fn member_node(item: Item, node: NodeId, name: &str) -> Option<NodeId> {
    let tree = item.tree;
    let declares = |line: NodeId| {
        tree.descendants(line).into_iter().any(|inner| {
            matches!(
                tree.rule(inner),
                Rule::Field
                    | Rule::Variant
                    | Rule::TagLine
                    | Rule::LayoutMember
                    | Rule::RegisterLine
                    | Rule::InterfaceMember
            ) && tree
                .child_tokens(inner)
                .find(|token| item.file.tokens[*token as usize].kind == TokenKind::Ident)
                .is_some_and(|token| item.file.text(token as usize) == name)
        })
    };
    tree.child_nodes(node)
        .filter(|line| {
            matches!(
                tree.rule(*line),
                Rule::StructMember
                    | Rule::EnumMember
                    | Rule::TagLine
                    | Rule::LayoutMember
                    | Rule::RegisterMember
                    | Rule::InterfaceMember
            )
        })
        .find(|line| declares(*line))
}
