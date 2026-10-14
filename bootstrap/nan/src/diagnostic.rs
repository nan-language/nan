//! Diagnostic codes, their classes, and the diagnostics the toolchain reports.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Error,
    Incompleteness,
    Internal,
}

impl Class {
    /// The word starting a diagnostic record of this class.
    pub fn word(self) -> &'static str {
        match self {
            Class::Error => "error",
            Class::Incompleteness => "incomplete",
            Class::Internal => "internal",
        }
    }
}

macro_rules! codes {
    ($($variant:ident = $name:literal,)*) => {
        /// The diagnostic codes this implementation reports.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Code {
            $($variant,)*
        }

        impl Code {
            pub fn name(self) -> &'static str {
                match self {
                    $(Code::$variant => $name,)*
                }
            }

            pub fn from_name(name: &str) -> Option<Code> {
                match name {
                    $($name => Some(Code::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

codes! {
    BuildSymbolicLink = "build.symbolic-link",
    DocMisplaced = "doc.misplaced",
    DocModuleFileMissing = "doc.module-file-missing",
    DocSummaryMissing = "doc.summary-missing",
    EffectUsesUnknown = "effect.uses-unknown",
    ExprChainedComparison = "expr.chained-comparison",
    ExprMarkerOrder = "expr.marker-order",
    ExprParentheses = "expr.parentheses",
    InternalInvariant = "internal.invariant",
    ItemBodyOpener = "item.body-opener",
    ItemClauseOrder = "item.clause-order",
    ItemConstUntyped = "item.const-untyped",
    ItemForeignForm = "item.foreign-form",
    ItemIndented = "item.indented",
    ItemLabelMismatch = "item.label-mismatch",
    ItemMissingBody = "item.missing-body",
    ItemModifierConflict = "item.modifier-conflict",
    ItemModifierMisplaced = "item.modifier-misplaced",
    ItemMultipleLines = "item.multiple-lines",
    ItemOpennessMissing = "item.openness-missing",
    ItemOpennessNotPub = "item.openness-not-pub",
    ItemReservedWord = "item.reserved-word",
    ItemSelfFieldSet = "item.self-field-set",
    ItemSelfMisplaced = "item.self-misplaced",
    ItemStrayColumnZero = "item.stray-column-zero",
    ItemTransparentLimits = "item.transparent-limits",
    ItemUnitResult = "item.unit-result",
    ItemVisibilityNotAllowed = "item.visibility-not-allowed",
    LanguageUnsupported = "language.unsupported",
    LanguageVersionForm = "language.version-form",
    LexCarriageReturn = "lex.carriage-return",
    LexCharLiteralSize = "lex.char-literal-size",
    LexEscapeOutOfRange = "lex.escape-out-of-range",
    LexIndentation = "lex.indentation",
    LexInvalidUtf8 = "lex.invalid-utf8",
    LexLiteralSuffix = "lex.literal-suffix",
    LexMalformedNumber = "lex.malformed-number",
    LexMissingFinalNewline = "lex.missing-final-newline",
    LexNonAscii = "lex.non-ascii",
    LexNonAsciiByte = "lex.non-ascii-byte",
    LexNotAtLineStart = "lex.not-at-line-start",
    LexRejectedCodePoint = "lex.rejected-code-point",
    LexTab = "lex.tab",
    LexTrailingWhitespace = "lex.trailing-whitespace",
    LexUnexpectedCharacter = "lex.unexpected-character",
    LexUnknownEscape = "lex.unknown-escape",
    LexUnterminatedLiteral = "lex.unterminated-literal",
    ManifestAnyNotAlone = "manifest.any-not-alone",
    ManifestBadSource = "manifest.bad-source",
    ManifestDenySelf = "manifest.deny-self",
    ManifestDuplicate = "manifest.duplicate",
    ManifestMisplaced = "manifest.misplaced",
    ManifestMissing = "manifest.missing",
    ManifestMissingDirective = "manifest.missing-directive",
    ManifestReservedTarget = "manifest.reserved-target",
    ManifestUnknownTarget = "manifest.unknown-target",
    ModuleVariantMissing = "module.variant-missing",
    ModuleVariantMixed = "module.variant-mixed",
    ModuleVariantOverlap = "module.variant-overlap",
    ModuleVariantUnmatched = "module.variant-unmatched",
    NameDotForPath = "name.dot-for-path",
    RegisterBankReserved = "register.bank-reserved",
    SyntaxForeign = "syntax.foreign",
    SyntaxMismatchedDelimiter = "syntax.mismatched-delimiter",
    SyntaxUnclosedDelimiter = "syntax.unclosed-delimiter",
    SyntaxUnexpectedToken = "syntax.unexpected-token",
    TaskWhenTake = "task.when-take",
    ToolNoWorkspace = "tool.no-workspace",
    ToolUnknownScope = "tool.unknown-scope",
    ToolUsage = "tool.usage",
}

impl Code {
    pub fn class(self) -> Class {
        match self {
            Code::InternalInvariant => Class::Internal,
            _ => Class::Error,
        }
    }
}

/// The role of a reason in a because-chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A value the owning rule names, on no line.
    Note,
}

impl Role {
    pub fn word(self) -> &'static str {
        match self {
            Role::Note => "note",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason {
    pub role: Role,
    pub subject: String,
}

impl Reason {
    pub fn note(subject: &str) -> Reason {
        Reason {
            role: Role::Note,
            subject: subject.to_owned(),
        }
    }
}

/// One reported root cause. The quote and the item are derived from the location when printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub expected: Option<String>,
    pub found: Option<String>,
    pub because: Vec<Reason>,
}

impl Diagnostic {
    pub fn new(code: Code) -> Diagnostic {
        Diagnostic {
            code,
            path: None,
            line: None,
            expected: None,
            found: None,
            because: Vec::new(),
        }
    }

    pub fn at(code: Code, path: &str, line: u32) -> Diagnostic {
        Diagnostic {
            path: Some(path.to_owned()),
            line: Some(line),
            ..Diagnostic::new(code)
        }
    }
}
