//! The manifest, the `package` item of `package.nan`, and the rules of its directives.

use crate::diagnostic::{Code, Diagnostic};
use crate::syntax::file::SourceFile;
use crate::syntax::grammar::Rule;
use crate::syntax::items::Item;
use crate::syntax::literal::text_value;
use crate::syntax::token::TokenKind;
use crate::syntax::tree::{NodeId, Tree};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    Host,
    Uefi,
    Bare,
    Any,
}

impl Target {
    pub fn name(self) -> &'static str {
        match self {
            Target::Host => "x86_64-host",
            Target::Uefi => "x86_64-uefi",
            Target::Bare => "x86_64-bare",
            Target::Any => "any",
        }
    }

    pub fn from_name(name: &str) -> Option<Target> {
        [Target::Host, Target::Uefi, Target::Bare, Target::Any]
            .into_iter()
            .find(|target| target.name() == name)
    }
}

pub struct Manifest {
    pub name: String,
    /// The line of the `package` keyword.
    pub line: u32,
    /// The declared targets this implementation knows.
    pub targets: Vec<Target>,
    pub targets_line: Option<u32>,
    /// Whether the package's language version is one this implementation implements.
    pub supported: bool,
    /// The language version the `language` directive states, and its line.
    pub language: Option<(u128, u32)>,
    /// The name the `entry` directive gives, and its line.
    pub entry: Option<(String, u32)>,
    pub deps: Vec<Dep>,
    pub allows: Vec<Allow>,
    /// The module pairs of `deny a -> b` directives, and their lines.
    pub denies: Vec<(String, String, u32)>,
}

pub struct Dep {
    pub alias: String,
    /// The workspace directory a relative source names, or the store name.
    pub source: Option<String>,
    /// The source as the directive writes it.
    pub written: String,
    pub path: bool,
    pub line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Authority {
    Device,
    Priv,
    Trusted,
    Validate,
    /// The registry path of a `contribute` grant, as written.
    Contribute(String),
}

impl Authority {
    /// The authority as an `allow` directive writes it.
    pub fn written(&self) -> String {
        match self {
            Authority::Device => "device".to_owned(),
            Authority::Priv => "priv".to_owned(),
            Authority::Trusted => "trusted".to_owned(),
            Authority::Validate => "validate".to_owned(),
            Authority::Contribute(registry) => format!("contribute {registry}"),
        }
    }
}

pub struct Allow {
    pub authority: Authority,
    pub names: Vec<String>,
    pub line: u32,
}

/// The manifest's directives, read from its tree.
struct Reader<'a> {
    file: &'a SourceFile,
    tree: &'a Tree,
    path: &'a str,
    package_dir: &'a str,
    found: Vec<Diagnostic>,
}

impl Manifest {
    /// Reads the manifest of `package.nan` at `path`, reporting its diagnostics. A file whose
    /// manifest the parse abandoned has none, reported by its syntax failure alone.
    pub fn read(
        file: &SourceFile,
        path: &str,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Manifest> {
        let item = file.items.iter().find(|item| {
            item.tree.as_ref().is_some_and(|tree| {
                Item { file, tree }
                    .kind()
                    .is_some_and(|(rule, _)| rule == Rule::PackageItem)
            })
        });
        let Some(item) = item else {
            diagnostics.push(Diagnostic::at(Code::ManifestMissing, path, 1));
            return None;
        };
        if item.abandoned {
            return None;
        }
        let tree = item.tree.as_ref().expect("a parsed item has a tree");
        let (_, node) = Item { file, tree }
            .kind()
            .expect("the manifest is a package item");
        let package_dir = path.strip_suffix("/package.nan").expect("a manifest path");
        let mut reader = Reader {
            file,
            tree,
            path,
            package_dir,
            found: Vec::new(),
        };
        let manifest = reader.read(node);
        diagnostics.extend(reader.found);
        Some(manifest)
    }
}

impl Reader<'_> {
    fn text(&self, token: u32) -> &str {
        self.file.text(token as usize)
    }

    fn line(&self, node: NodeId) -> u32 {
        self.file.tokens[self.tree.span(node).expect("a directive holds tokens").0 as usize].line
    }

    fn report(&mut self, line: u32, code: Code) {
        let diagnostic = Diagnostic::at(code, self.path, line);
        if !self.found.contains(&diagnostic) {
            self.found.push(diagnostic);
        }
    }

    /// The texts of the `IDENT` tokens directly under `node`, a directive's word included.
    fn idents(&self, node: NodeId) -> Vec<String> {
        self.tree
            .child_tokens(node)
            .filter(|token| self.file.tokens[*token as usize].kind == TokenKind::Ident)
            .map(|token| self.text(token).to_owned())
            .collect()
    }

    /// The text of a node's tokens, without spaces.
    fn joined(&self, node: NodeId) -> String {
        let (first, last) = self.tree.span(node).expect("a node holding tokens");
        (first..=last).map(|token| self.text(token)).collect()
    }

    fn read(&mut self, package: NodeId) -> Manifest {
        let name_token = self
            .tree
            .child_tokens(package)
            .find(|token| self.file.tokens[*token as usize].kind == TokenKind::Ident);
        let name = self
            .text(name_token.expect("a package names itself"))
            .to_owned();
        let keyword = self
            .tree
            .child_tokens(package)
            .next()
            .expect("the package keyword");
        let line = self.file.tokens[keyword as usize].line;
        let mut manifest = Manifest {
            name,
            line,
            targets: Vec::new(),
            targets_line: None,
            supported: true,
            language: None,
            entry: None,
            deps: Vec::new(),
            allows: Vec::new(),
            denies: Vec::new(),
        };
        let (mut language, mut entry) = (false, false);
        let mut dep_aliases: Vec<String> = Vec::new();
        let mut dep_sources: Vec<String> = Vec::new();
        let mut authorities: Vec<String> = Vec::new();
        let mut denials: Vec<(String, String)> = Vec::new();
        for directive in self
            .tree
            .child_nodes(package)
            .filter(|node| self.tree.rule(*node) == Rule::Directive)
        {
            let node = self
                .tree
                .child_nodes(directive)
                .next()
                .expect("a directive");
            let line = self.line(node);
            match self.tree.rule(node) {
                Rule::LanguageDirective => {
                    if std::mem::replace(&mut language, true) {
                        self.report(line, Code::ManifestDuplicate);
                        continue;
                    }
                    let version = self.tree.child_tokens(node).nth(1).expect("a version");
                    let text = self.text(version);
                    let decimal = text.chars().all(|c| c.is_ascii_digit() || c == '_');
                    manifest.supported = int_value(text) == Some(1);
                    manifest.language = int_value(text).map(|version| (version, line));
                    if !decimal {
                        self.report(line, Code::LanguageVersionForm);
                    }
                    if !manifest.supported && decimal {
                        self.report(line, Code::LanguageUnsupported);
                    }
                }
                Rule::TargetsDirective => {
                    if manifest.targets_line.replace(line).is_some() {
                        self.report(line, Code::ManifestDuplicate);
                        continue;
                    }
                    let list = self
                        .tree
                        .find(node, Rule::TargetList)
                        .expect("a target list");
                    let names: Vec<String> = self
                        .tree
                        .child_nodes(list)
                        .map(|target| self.joined(target))
                        .collect();
                    for name in &names {
                        match Target::from_name(name) {
                            Some(target) => manifest.targets.push(target),
                            None if name == "x86_64-nanos" => {
                                self.report(line, Code::ManifestReservedTarget)
                            }
                            None => self.report(line, Code::ManifestUnknownTarget),
                        }
                    }
                    if names.iter().any(|name| name == "any") && names.len() > 1 {
                        self.report(line, Code::ManifestAnyNotAlone);
                    }
                    if has_repeat(&names) {
                        self.report(line, Code::ManifestDuplicate);
                    }
                }
                Rule::EntryDirective => {
                    if std::mem::replace(&mut entry, true) {
                        self.report(line, Code::ManifestDuplicate);
                        continue;
                    }
                    manifest.entry = Some((self.idents(node).swap_remove(1), line));
                }
                Rule::DepDirective => {
                    let alias = self.idents(node).swap_remove(1);
                    let text = self
                        .tree
                        .child_tokens(node)
                        .find(|token| self.file.tokens[*token as usize].kind == TokenKind::Text);
                    let source = text_value(self.text(text.expect("a source")));
                    let resolved = match source_kind(&source) {
                        Some(Source::Path) => Some(resolve(self.package_dir, &source)),
                        Some(Source::Store) => Some(source.clone()),
                        None => {
                            self.report(line, Code::ManifestBadSource);
                            None
                        }
                    };
                    let repeated = dep_aliases.contains(&alias)
                        || resolved
                            .as_ref()
                            .is_some_and(|key| dep_sources.contains(key));
                    if repeated {
                        self.report(line, Code::ManifestDuplicate);
                    }
                    manifest.deps.push(Dep {
                        alias: alias.clone(),
                        source: resolved.clone(),
                        written: source.clone(),
                        path: matches!(source_kind(&source), Some(Source::Path)),
                        line,
                    });
                    dep_aliases.push(alias);
                    dep_sources.extend(resolved);
                }
                Rule::AllowDirective => {
                    let authority =
                        self.joined(self.tree.find(node, Rule::Authority).expect("an authority"));
                    let names =
                        self.idents(self.tree.find(node, Rule::NameList).expect("a name list"));
                    if authorities.contains(&authority) || has_repeat(&names) {
                        self.report(line, Code::ManifestDuplicate);
                    }
                    let granted = match authority.as_str() {
                        "device" => Authority::Device,
                        "priv" => Authority::Priv,
                        "trusted" => Authority::Trusted,
                        "validate" => Authority::Validate,
                        _ => Authority::Contribute(authority["contribute".len()..].to_owned()),
                    };
                    manifest.allows.push(Allow {
                        authority: granted,
                        names,
                        line,
                    });
                    authorities.push(authority);
                }
                Rule::DenyDirective => {
                    let modules = self.idents(node);
                    let pair = (modules[1].clone(), modules[2].clone());
                    if pair.0 == pair.1 {
                        self.report(line, Code::ManifestDenySelf);
                    }
                    if denials.contains(&pair) {
                        self.report(line, Code::ManifestDuplicate);
                    }
                    manifest.denies.push((pair.0.clone(), pair.1.clone(), line));
                    denials.push(pair);
                }
                rule => unreachable!("{} is no directive", rule.name()),
            }
        }
        if !language || manifest.targets_line.is_none() {
            self.report(manifest.line, Code::ManifestMissingDirective);
        }
        manifest
    }
}

fn has_repeat(names: &[String]) -> bool {
    names
        .iter()
        .enumerate()
        .any(|(index, name)| names[..index].contains(name))
}

enum Source {
    Path,
    Store,
}

/// Whether a `dep` source is a relative path or a store name.
fn source_kind(source: &str) -> Option<Source> {
    let mut segments = source.split('/');
    let first = segments.next().expect("split yields a segment");
    if first == "." || first == ".." {
        let segment = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        };
        return segments.all(segment).then_some(Source::Path);
    }
    let segment = |s: &str| {
        s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && s.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_')
            })
    };
    source.split('/').all(segment).then_some(Source::Store)
}

/// The workspace path a relative source names from the package directory, `.` and `..` resolved.
pub fn resolve(package_dir: &str, source: &str) -> String {
    let mut parts: Vec<&str> = package_dir.split('/').collect();
    for segment in source.split('/') {
        match segment {
            "." => {}
            ".." if parts.last().is_some_and(|part| *part != "..") => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    parts.join("/")
}

/// The value of an `INT` token, in any base, without separators and suffix.
fn int_value(text: &str) -> Option<u128> {
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    let (radix, digits) = match digits.strip_prefix("0x") {
        Some(hex) => (16, hex.to_owned()),
        None => match digits.strip_prefix("0b") {
            Some(binary) => (2, binary.to_owned()),
            None => (10, digits),
        },
    };
    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    u128::from_str_radix(&digits[..end], radix).ok()
}

/// The relative path source naming the directory `to` from the directory `from`, both relative
/// to the workspace root.
pub fn relative(from: &str, to: &str) -> String {
    let from: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
    let to: Vec<&str> = to.split('/').filter(|part| !part.is_empty()).collect();
    let shared = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts: Vec<&str> = vec![".."; from.len() - shared];
    parts.extend(&to[shared..]);
    match parts.first() {
        Some(&"..") => parts.join("/"),
        _ => {
            let mut written = String::from(".");
            for part in parts {
                written.push('/');
                written.push_str(part);
            }
            written
        }
    }
}
