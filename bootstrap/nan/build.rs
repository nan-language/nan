//! Generates the parser's grammar tables from the `ebnf` blocks of the published grammar, the std
//! declarations from the specification's `nan-api` blocks, and the hash of the toolchain source
//! tree.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path;
use std::{env, fs};

// The crate formats these shared modules; formatting them from here would look for their tests
// beside the build script.
#[path = "src/encoding.rs"]
#[rustfmt::skip]
mod encoding;
#[allow(dead_code, reason = "the crate shares this module and uses the hash forms the build does not")]
#[path = "src/hash.rs"]
#[rustfmt::skip]
mod hash;

const SPEC_PATH: &str = "docs/spec";
const GRAMMAR_PATH: &str = "docs/spec/41-grammar.md";
const START: &str = "item";

const PUNCT_NAMES: &[(&str, &str)] = &[
    ("(", "LParen"),
    (")", "RParen"),
    ("[", "LBracket"),
    ("]", "RBracket"),
    ("{", "LBrace"),
    ("}", "RBrace"),
    (",", "Comma"),
    (":", "Colon"),
    ("::", "ColonColon"),
    (".", "Dot"),
    ("->", "Arrow"),
    ("=>", "FatArrow"),
    ("=", "Eq"),
    ("+=", "PlusEq"),
    ("-=", "MinusEq"),
    ("*=", "StarEq"),
    ("/=", "SlashEq"),
    ("%=", "PercentEq"),
    ("&=", "AmpEq"),
    ("|=", "PipeEq"),
    ("^=", "CaretEq"),
    ("<<=", "ShlEq"),
    (">>=", "ShrEq"),
    ("+", "Plus"),
    ("-", "Minus"),
    ("*", "Star"),
    ("/", "Slash"),
    ("%", "Percent"),
    ("&", "Amp"),
    ("|", "Pipe"),
    ("^", "Caret"),
    ("~", "Tilde"),
    ("<<", "Shl"),
    (">>", "Shr"),
    ("==", "EqEq"),
    ("!=", "NotEq"),
    ("<", "Lt"),
    ("<=", "Le"),
    (">", "Gt"),
    (">=", "Ge"),
    ("..<", "DotDotLt"),
    ("..=", "DotDotEq"),
    ("&&", "AmpAmp"),
    ("||", "PipePipe"),
    ("!", "Bang"),
    ("?", "Question"),
    (";", "Semi"),
    ("..", "DotDot"),
    ("--", "MinusMinus"),
];

/// The files the toolchain is built from, relative to the repository root: directories stand for
/// every file under them.
const SOURCE_TREE: &[&str] = &[
    "Cargo.lock",
    "Cargo.toml",
    "bootstrap/nan/Cargo.toml",
    "bootstrap/nan/build.rs",
    "bootstrap/nan/src",
    "docs/spec",
    "rust-toolchain.toml",
];

fn main() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).to_path_buf();
    let grammar = repository.join(GRAMMAR_PATH);
    let text = fs::read_to_string(&grammar).expect("read the published grammar");
    let rules = parse_grammar(&text);
    fs::write(out.join("grammar.rs"), Generator::new(&rules).generate())
        .expect("write the grammar tables");
    fs::write(
        out.join("std_api.rs"),
        std_declarations(&repository.join(SPEC_PATH)),
    )
    .expect("write the std declarations");

    let mut files = Vec::new();
    for entry in SOURCE_TREE {
        println!(
            "cargo::rerun-if-changed={}",
            repository.join(entry).display()
        );
        collect(&repository, entry, &mut files);
    }
    files.sort();
    let mut list = Vec::new();
    encoding::uvar(&mut list, files.len() as u128);
    for file in &files {
        encoding::bytes(&mut list, file.as_bytes());
        encoding::bytes(
            &mut list,
            &fs::read(repository.join(file)).expect("read a toolchain source file"),
        );
    }
    let tree = hash::Hash::of(&list).0;
    fs::write(
        out.join("toolchain.rs"),
        format!("const SOURCE_TREE: [u8; 33] = {tree:?};\n"),
    )
    .expect("write the source tree hash");
}

/// The text of every `nan-api` block of the specification, the blocks of each std module joined
/// in chapter order, as the constant `DECLARATIONS` of module paths and texts.
fn std_declarations(spec: &Path) -> String {
    let mut chapters: Vec<_> = fs::read_dir(spec)
        .expect("read the specification")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .collect();
    chapters.sort();
    let mut modules: Vec<(String, String)> = Vec::new();
    for chapter in chapters {
        let text = fs::read_to_string(&chapter).expect("read a chapter");
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            if line != "```nan-api" {
                continue;
            }
            let header = lines.next().expect("a block names its module");
            let module = header
                .strip_prefix("// module: ")
                .expect("a block starts with its module line");
            let block: Vec<&str> = lines.by_ref().take_while(|line| *line != "```").collect();
            let index = match modules.iter().position(|(name, _)| name == module) {
                Some(index) => index,
                None => {
                    modules.push((module.to_owned(), String::new()));
                    modules.len() - 1
                }
            };
            let text = &mut modules[index].1;
            for line in block {
                text.push_str(line);
                text.push('\n');
            }
            text.push('\n');
        }
    }
    let mut out =
        String::from("// Generated by build.rs from the nan-api blocks of docs/spec.\n\n");
    writeln!(
        out,
        "pub const DECLARATIONS: [(&str, &str); {}] = {modules:?};",
        modules.len()
    )
    .unwrap();
    out
}

/// Adds the files at `path`, a file or a directory, to `files`.
fn collect(repository: &Path, path: &str, files: &mut Vec<String>) {
    let absolute = repository.join(path);
    if !absolute.is_dir() {
        files.push(path.to_owned());
        return;
    }
    for entry in fs::read_dir(absolute).expect("read a toolchain source directory") {
        let name = entry
            .expect("a directory entry")
            .file_name()
            .into_string()
            .expect("a UTF-8 file name");
        collect(repository, &format!("{path}/{name}"), files);
    }
}

#[derive(Clone, Debug)]
enum Atom {
    Name(String),
    Quoted(String),
    Context(String),
    CodePoints(u32, u32),
    Group(Vec<Seq>),
}

#[derive(Clone, Copy, Debug)]
enum Repeat {
    One,
    Opt,
    Star,
    Plus,
}

#[derive(Clone, Debug)]
struct Elem {
    atom: Atom,
    repeat: Repeat,
}

#[derive(Clone, Debug)]
struct Seq {
    elems: Vec<Elem>,
    rejection: Option<String>,
}

#[derive(Debug)]
enum Tok {
    Name(String),
    Quoted(String),
    Context(String),
    CodePoint(u32),
    Rejection(String),
    Sym(&'static str),
}

/// Parses every rule of the `ebnf` blocks, written in the grammar notation of the charter.
fn parse_grammar(text: &str) -> HashMap<String, Vec<Seq>> {
    let mut source = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line == "```ebnf" {
            inside = true;
        } else if line.starts_with("```") {
            inside = false;
        } else if inside {
            source.push_str(line);
            source.push('\n');
        }
    }
    let mut parser = EbnfParser {
        toks: tokenize(&source),
        pos: 0,
    };
    let mut rules = HashMap::new();
    while parser.pos < parser.toks.len() {
        let name = match parser.bump() {
            Tok::Name(name) => name.clone(),
            tok => panic!("rule name expected, found {tok:?}"),
        };
        parser.expect("=");
        let body = parser.choice();
        parser.expect(";");
        assert!(
            rules.insert(name.clone(), body).is_none(),
            "rule {name} defined twice"
        );
    }
    rules
}

fn tokenize(source: &str) -> Vec<Tok> {
    let chars: Vec<char> = source.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    let take_until = |i: &mut usize, end: char| -> String {
        let start = *i;
        while chars[*i] != end {
            *i += 1;
        }
        let text = chars[start..*i].iter().collect();
        *i += 1;
        text
    };
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == 'U' && chars.get(i + 1) == Some(&'+') {
            i += 2;
            let start = i;
            while i < chars.len() && chars[i].is_ascii_hexdigit() {
                i += 1;
            }
            let digits: String = chars[start..i].iter().collect();
            toks.push(Tok::CodePoint(
                u32::from_str_radix(&digits, 16).expect("hex code point"),
            ));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            toks.push(Tok::Name(chars[start..i].iter().collect()));
        } else if c == '"' || c == '\'' {
            i += 1;
            let word = take_until(&mut i, c);
            toks.push(if c == '"' {
                Tok::Quoted(word)
            } else {
                Tok::Context(word)
            });
        } else if c == '{' && chars.get(i + 1) == Some(&'!') {
            i += 2;
            toks.push(Tok::Rejection(take_until(&mut i, '}')));
        } else if c == '.' && chars.get(i + 1) == Some(&'.') {
            toks.push(Tok::Sym(".."));
            i += 2;
        } else {
            let sym = ["=", ";", "|", "?", "*", "+", "-", "(", ")"]
                .into_iter()
                .find(|s| s.starts_with(c))
                .unwrap_or_else(|| panic!("unexpected character {c:?} in the grammar"));
            toks.push(Tok::Sym(sym));
            i += 1;
        }
    }
    toks
}

struct EbnfParser {
    toks: Vec<Tok>,
    pos: usize,
}

impl EbnfParser {
    fn bump(&mut self) -> &Tok {
        self.pos += 1;
        &self.toks[self.pos - 1]
    }

    fn at_sym(&self, sym: &str) -> bool {
        matches!(self.toks.get(self.pos), Some(Tok::Sym(s)) if *s == sym)
    }

    fn expect(&mut self, sym: &str) {
        assert!(
            self.at_sym(sym),
            "{sym:?} expected at grammar token {}",
            self.pos
        );
        self.pos += 1;
    }

    fn choice(&mut self) -> Vec<Seq> {
        let mut alts = vec![self.sequence()];
        while self.at_sym("|") {
            self.pos += 1;
            alts.push(self.sequence());
        }
        alts
    }

    fn sequence(&mut self) -> Seq {
        let mut elems = Vec::new();
        while self.starts_atom() {
            let atom = self.atom();
            let repeat = if self.at_sym("?") {
                Repeat::Opt
            } else if self.at_sym("*") {
                Repeat::Star
            } else if self.at_sym("+") {
                Repeat::Plus
            } else {
                Repeat::One
            };
            if !matches!(repeat, Repeat::One) {
                self.pos += 1;
            } else if self.at_sym("-") {
                self.pos += 1;
                self.atom();
            }
            elems.push(Elem { atom, repeat });
        }
        assert!(
            !elems.is_empty(),
            "empty sequence at grammar token {}",
            self.pos
        );
        let rejection = match self.toks.get(self.pos) {
            Some(Tok::Rejection(code)) => {
                let code = code.clone();
                self.pos += 1;
                Some(code)
            }
            _ => None,
        };
        Seq { elems, rejection }
    }

    fn starts_atom(&self) -> bool {
        match self.toks.get(self.pos) {
            Some(Tok::Name(_) | Tok::Quoted(_) | Tok::Context(_) | Tok::CodePoint(_)) => true,
            Some(Tok::Sym(s)) => *s == "(",
            _ => false,
        }
    }

    fn atom(&mut self) -> Atom {
        let atom = match self.bump() {
            Tok::Name(name) => Atom::Name(name.clone()),
            Tok::Quoted(text) => Atom::Quoted(text.clone()),
            Tok::Context(word) => Atom::Context(word.clone()),
            Tok::CodePoint(value) => Atom::CodePoints(*value, *value),
            Tok::Sym("(") => {
                let alts = self.choice();
                self.expect(")");
                return Atom::Group(alts);
            }
            tok => panic!("atom expected, found {tok:?}"),
        };
        if !self.at_sym("..") {
            return atom;
        }
        self.pos += 1;
        match (atom, self.atom()) {
            (Atom::CodePoints(low, _), Atom::CodePoints(_, high)) => Atom::CodePoints(low, high),
            (atom, _) => atom,
        }
    }
}

fn camel(name: &str) -> String {
    name.split(['_', '.', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let first = chars.next().expect("non-empty part").to_ascii_uppercase();
            std::iter::once(first)
                .chain(chars.map(|c| c.to_ascii_lowercase()))
                .collect::<String>()
        })
        .collect()
}

fn keyword_variant(word: &str) -> String {
    match word {
        "_" => "Underscore".to_owned(),
        "self" => "SelfValue".to_owned(),
        _ => camel(word),
    }
}

fn punct_variant(spelling: &str) -> &'static str {
    PUNCT_NAMES
        .iter()
        .find(|(s, _)| *s == spelling)
        .map(|(_, name)| *name)
        .unwrap_or_else(|| panic!("no name for punctuation {spelling:?}"))
}

/// The quoted alternatives of a lexical rule listing spellings, such as `PUNCT`.
fn spellings(rules: &HashMap<String, Vec<Seq>>, name: &str) -> Vec<String> {
    rules[name]
        .iter()
        .map(|seq| match &seq.elems[..] {
            [
                Elem {
                    atom: Atom::Quoted(text),
                    repeat: Repeat::One,
                },
            ] => text.clone(),
            _ => panic!("{name} lists only quoted spellings"),
        })
        .collect()
}

struct Generator<'a> {
    rules: &'a HashMap<String, Vec<Seq>>,
    keywords: Vec<String>,
    punct: Vec<String>,
    foreign: Vec<String>,
    order: Vec<String>,
    context_words: BTreeSet<String>,
    choices: Vec<Vec<usize>>,
    seqs: Vec<String>,
}

impl<'a> Generator<'a> {
    fn new(rules: &'a HashMap<String, Vec<Seq>>) -> Self {
        let mut generator = Generator {
            rules,
            keywords: spellings(rules, "HARD_KEYWORD"),
            punct: spellings(rules, "PUNCT"),
            foreign: spellings(rules, "FOREIGN"),
            order: Vec::new(),
            context_words: BTreeSet::new(),
            choices: Vec::new(),
            seqs: Vec::new(),
        };
        generator.reach(START);
        generator
    }

    /// Lists the syntactic rules reachable from `name`, in order of first reach.
    fn reach(&mut self, name: &str) {
        if self.order.iter().any(|rule| rule == name) {
            return;
        }
        self.order.push(name.to_owned());
        let body = self
            .rules
            .get(name)
            .unwrap_or_else(|| panic!("undefined rule {name}"));
        self.reach_seqs(body);
    }

    fn reach_seqs(&mut self, seqs: &[Seq]) {
        for seq in seqs {
            for elem in &seq.elems {
                match &elem.atom {
                    Atom::Name(name) if is_rule_name(name) => self.reach(name),
                    Atom::Context(word) => {
                        self.context_words.insert(word.clone());
                    }
                    Atom::Group(alts) => self.reach_seqs(alts),
                    _ => {}
                }
            }
        }
    }

    fn generate(mut self) -> String {
        let mut out = String::new();
        out.push_str("// Generated by build.rs from docs/spec/41-grammar.md.\n\n");
        spelling_enum(&mut out, "Keyword", &self.keywords, keyword_variant, true);
        let punct = |s: &str| punct_variant(s).to_owned();
        spelling_enum(&mut out, "Punct", &self.punct, punct, false);
        spelling_enum(&mut out, "Foreign", &self.foreign, punct, false);
        let words: Vec<String> = self.context_words.iter().cloned().collect();
        spelling_enum(&mut out, "ContextWord", &words, camel, true);

        for name in ["REJECTED", "REJECTED_SPACE"] {
            let mut ranges = code_point_ranges(self.rules, name);
            ranges.sort_unstable();
            writeln!(
                out,
                "pub static {name}: [(u32, u32); {}] = {:?};\n",
                ranges.len(),
                ranges
            )
            .unwrap();
        }

        let rule_choices: Vec<usize> = self
            .order
            .clone()
            .iter()
            .map(|name| self.choice(&self.rules[name].clone()))
            .collect();
        out.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n");
        out.push_str("pub enum Rule {\n");
        for name in &self.order {
            writeln!(out, "    {},", camel(name)).unwrap();
        }
        out.push_str("}\n\nimpl Rule {\n");
        out.push_str("    pub fn name(self) -> &'static str {\n        match self {\n");
        for name in &self.order {
            writeln!(out, "            Rule::{} => {:?},", camel(name), name).unwrap();
        }
        out.push_str("        }\n    }\n\n    pub fn choice(self) -> ChoiceId {\n");
        out.push_str("        RULE_CHOICES[self as usize]\n    }\n}\n\n");
        writeln!(
            out,
            "static RULE_CHOICES: [ChoiceId; {}] = {:?};\n",
            rule_choices.len(),
            rule_choices
        )
        .unwrap();
        writeln!(
            out,
            "pub static CHOICES: [&[SeqId]; {}] = [",
            self.choices.len()
        )
        .unwrap();
        for alts in &self.choices {
            writeln!(out, "    &{alts:?},").unwrap();
        }
        out.push_str("];\n\n");
        writeln!(out, "pub static SEQS: [SeqDef; {}] = [", self.seqs.len()).unwrap();
        for seq in &self.seqs {
            writeln!(out, "    {seq},").unwrap();
        }
        out.push_str("];\n");
        out
    }

    /// Registers a choice and its alternatives, returning its id.
    fn choice(&mut self, alts: &[Seq]) -> usize {
        let id = self.choices.len();
        self.choices.push(Vec::new());
        let seq_ids = alts.iter().map(|seq| self.seq(seq)).collect();
        self.choices[id] = seq_ids;
        id
    }

    fn seq(&mut self, seq: &Seq) -> usize {
        let id = self.seqs.len();
        self.seqs.push(String::new());
        let elems: Vec<String> = seq.elems.iter().map(|elem| self.elem(elem)).collect();
        let rejection = match &seq.rejection {
            Some(code) => format!("Some(Code::{})", camel(code)),
            None => "None".to_owned(),
        };
        self.seqs[id] = format!(
            "SeqDef {{ elems: &[{}], rejection: {rejection} }}",
            elems.join(", ")
        );
        id
    }

    fn elem(&mut self, elem: &Elem) -> String {
        let atom = match &elem.atom {
            Atom::Name(name) if is_rule_name(name) => format!("Atom::Rule(Rule::{})", camel(name)),
            Atom::Name(name) => format!("Atom::Term(Term::Class(Class::{}))", camel(name)),
            Atom::Quoted(text) if is_word(text) => {
                assert!(
                    self.keywords.contains(text),
                    "{text:?} is not a hard keyword"
                );
                format!(
                    "Atom::Term(Term::Keyword(Keyword::{}))",
                    keyword_variant(text)
                )
            }
            Atom::Quoted(text) => {
                assert!(
                    self.punct.contains(text),
                    "{text:?} is not a punctuation token"
                );
                format!("Atom::Term(Term::Punct(Punct::{}))", punct_variant(text))
            }
            Atom::Context(word) => {
                format!("Atom::Term(Term::Context(ContextWord::{}))", camel(word))
            }
            Atom::Group(alts) => format!("Atom::Group({})", self.choice(alts)),
            Atom::CodePoints(..) => panic!("code points appear only in lexical rules"),
        };
        let repeat = match elem.repeat {
            Repeat::One => "One",
            Repeat::Opt => "Opt",
            Repeat::Star => "Star",
            Repeat::Plus => "Plus",
        };
        format!("ElemDef {{ atom: {atom}, repeat: Repeat::{repeat} }}")
    }
}

/// Writes an enum of the spellings a lexical rule lists, and with `lookup` the conversion from a
/// spelling.
fn spelling_enum(
    out: &mut String,
    name: &str,
    spellings: &[String],
    variant: impl Fn(&str) -> String,
    lookup: bool,
) {
    out.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n");
    writeln!(out, "pub enum {name} {{").unwrap();
    for spelling in spellings {
        writeln!(out, "    {},", variant(spelling)).unwrap();
    }
    writeln!(out, "}}\n\nimpl {name} {{").unwrap();
    writeln!(out, "    pub const ALL: [{name}; {}] = [", spellings.len()).unwrap();
    for spelling in spellings {
        writeln!(out, "        {name}::{},", variant(spelling)).unwrap();
    }
    out.push_str("    ];\n\n    pub fn spelling(self) -> &'static str {\n        match self {\n");
    for spelling in spellings {
        writeln!(
            out,
            "            {name}::{} => {spelling:?},",
            variant(spelling)
        )
        .unwrap();
    }
    out.push_str("        }\n    }\n");
    if !lookup {
        out.push_str("}\n\n");
        return;
    }
    writeln!(
        out,
        "\n    pub fn from_spelling(text: &str) -> Option<{name}> {{"
    )
    .unwrap();
    out.push_str("        match text {\n");
    for spelling in spellings {
        writeln!(
            out,
            "            {spelling:?} => Some({name}::{}),",
            variant(spelling)
        )
        .unwrap();
    }
    out.push_str("            _ => None,\n        }\n    }\n}\n\n");
}

/// The code point ranges a lexical rule matches, following the rules it names.
fn code_point_ranges(rules: &HashMap<String, Vec<Seq>>, name: &str) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    for seq in &rules[name] {
        for elem in &seq.elems {
            match &elem.atom {
                Atom::CodePoints(low, high) => ranges.push((*low, *high)),
                Atom::Name(name) => ranges.extend(code_point_ranges(rules, name)),
                atom => panic!("{name} holds {atom:?}, not code points"),
            }
        }
    }
    ranges
}

fn is_rule_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
}

fn is_word(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
