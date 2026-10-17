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
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    ApiStale = "api.stale",
    ApiWeakeningPending = "api.weakening-pending",
    ArtefactMalformed = "artefact.malformed",
    ArtefactRecheckFailed = "artefact.recheck-failed",
    ArtefactReservedSection = "artefact.reserved-section",
    ArtefactRootMismatch = "artefact.root-mismatch",
    ArtefactSymbolCollision = "artefact.symbol-collision",
    ArtefactVersionUnsupported = "artefact.version-unsupported",
    AuthorityCapabilityImpl = "authority.capability-impl",
    AuthorityDeniedDependency = "authority.denied-dependency",
    AuthorityDependencyUngranted = "authority.dependency-ungranted",
    AuthorityUngranted = "authority.ungranted",
    AuthorityUnknownName = "authority.unknown-name",
    AuthorityWideningUnapproved = "authority.widening-unapproved",
    BuildCacheExposed = "build.cache-exposed",
    BuildImageUnreadable = "build.image-unreadable",
    BuildStageMismatch = "build.stage-mismatch",
    BuildSymbolicLink = "build.symbolic-link",
    ClosureCaptureDuplicate = "closure.capture-duplicate",
    ClosureCaptureMode = "closure.capture-mode",
    ClosureCaptureUnused = "closure.capture-unused",
    ClosureProjectionValue = "closure.projection-value",
    ClosureSecondClass = "closure.second-class",
    ClosureShortEffect = "closure.short-effect",
    ComponentNotComponent = "component.not-component",
    ComponentStateGeneric = "component.state-generic",
    ComponentStateNotEncode = "component.state-not-encode",
    ComponentStateNotInert = "component.state-not-inert",
    ComponentUnavailable = "component.unavailable",
    ConstAssertFalse = "const.assert-false",
    ConstStepsExhausted = "const.steps-exhausted",
    ConstTrapped = "const.trapped",
    ConstTrustedViolation = "const.trusted-violation",
    ConstUninterpretable = "const.uninterpretable",
    ConstViewNotStatic = "const.view-not-static",
    ContractAssumeUntrusted = "contract.assume-untrusted",
    ContractDecreasesUnneeded = "contract.decreases-unneeded",
    ContractDecreasesUnproven = "contract.decreases-unproven",
    ContractEmptyReason = "contract.empty-reason",
    ContractIllDefined = "contract.ill-defined",
    ContractImplRequires = "contract.impl-requires",
    ContractImpureCondition = "contract.impure-condition",
    ContractOldForm = "contract.old-form",
    ContractQuantifierOutside = "contract.quantifier-outside",
    ContractTakenParameter = "contract.taken-parameter",
    ContractVacuous = "contract.vacuous",
    DeferAbandonHead = "defer.abandon-head",
    DeferDeferredPlaceMoved = "defer.deferred-place-moved",
    DeferFails = "defer.fails",
    DeferHasResult = "defer.has-result",
    DeferNotCall = "defer.not-call",
    DeferOperandForm = "defer.operand-form",
    DeferUndeferUnmatched = "defer.undefer-unmatched",
    DeferWaits = "defer.waits",
    DepCycle = "dep.cycle",
    DepEpochUnapproved = "dep.epoch-unapproved",
    DepLockStale = "dep.lock-stale",
    DepReleaseMissing = "dep.release-missing",
    DepSplitPackage = "dep.split-package",
    DepUnresolved = "dep.unresolved",
    DeriveConstituentMissing = "derive.constituent-missing",
    DeriveDuplicate = "derive.duplicate",
    DeriveEnumDefault = "derive.enum-default",
    DeriveUnknown = "derive.unknown",
    DocCodeBlock = "doc.code-block",
    DocDigestClaim = "doc.digest-claim",
    DocMisplaced = "doc.misplaced",
    DocModuleFileMissing = "doc.module-file-missing",
    DocPrivateReference = "doc.private-reference",
    DocStale = "doc.stale",
    DocSummaryMissing = "doc.summary-missing",
    DocUnresolvedReference = "doc.unresolved-reference",
    DocVariantMismatch = "doc.variant-mismatch",
    DomainUnavailable = "domain.unavailable",
    EditCheckFailed = "edit.check-failed",
    EditIdentityChanged = "edit.identity-changed",
    EditMoveInvalid = "edit.move-invalid",
    EditNotOneItem = "edit.not-one-item",
    EditStale = "edit.stale",
    EffectAwaitMissing = "effect.await-missing",
    EffectAwaitUnneeded = "effect.await-unneeded",
    EffectFitFails = "effect.fit-fails",
    EffectFitHead = "effect.fit-head",
    EffectFitKind = "effect.fit-kind",
    EffectFitWait = "effect.fit-wait",
    EffectForeignWait = "effect.foreign-wait",
    EffectUsesUnknown = "effect.uses-unknown",
    EncodeLinear = "encode.linear",
    EncodeManualImpl = "encode.manual-impl",
    EncodeNotInert = "encode.not-inert",
    EncodeView = "encode.view",
    ErrorCancelUnmapped = "error.cancel-unmapped",
    ErrorFailureType = "error.failure-type",
    ErrorFromAmbiguous = "error.from-ambiguous",
    ErrorFromDuplicate = "error.from-duplicate",
    ErrorFromShape = "error.from-shape",
    ErrorMarkerUnneeded = "error.marker-unneeded",
    ErrorUnconvertible = "error.unconvertible",
    ErrorUnhandled = "error.unhandled",
    ExampleFailed = "example.failed",
    ExampleForm = "example.form",
    ExampleTrapped = "example.trapped",
    ExprArgumentCount = "expr.argument-count",
    ExprArgumentLabel = "expr.argument-label",
    ExprBranchForm = "expr.branch-form",
    ExprChainedComparison = "expr.chained-comparison",
    ExprFieldCall = "expr.field-call",
    ExprFloatPattern = "expr.float-pattern",
    ExprGuardOutsideFirst = "expr.guard-outside-first",
    ExprMarkerOrder = "expr.marker-order",
    ExprMarkerTarget = "expr.marker-target",
    ExprMethodValue = "expr.method-value",
    ExprMisplacedJump = "expr.misplaced-jump",
    ExprMissingElse = "expr.missing-else",
    ExprNonExhaustive = "expr.non-exhaustive",
    ExprOrBindings = "expr.or-bindings",
    ExprOverlappingArms = "expr.overlapping-arms",
    ExprParentheses = "expr.parentheses",
    ExprRangeType = "expr.range-type",
    ExprUnreachableArm = "expr.unreachable-arm",
    FormatMachineInvalid = "format.machine-invalid",
    FormatMachineVersion = "format.machine-version",
    FormatReservedField = "format.reserved-field",
    GenericArgumentKind = "generic.argument-kind",
    GenericArity = "generic.arity",
    GenericConstType = "generic.const-type",
    GenericParameterUnused = "generic.parameter-unused",
    GenericUninferred = "generic.uninferred",
    GenericUnsatisfiedBound = "generic.unsatisfied-bound",
    GenericWhereMisplaced = "generic.where-misplaced",
    GenericWhereUnmet = "generic.where-unmet",
    HwAtomicType = "hw.atomic-type",
    HwDmaCompletion = "hw.dma-completion",
    HwDmaOperand = "hw.dma-operand",
    HwLandingCalled = "hw.landing-called",
    HwLandingSignature = "hw.landing-signature",
    HwLayoutUnfixed = "hw.layout-unfixed",
    HwMintRootMisplaced = "hw.mint-root-misplaced",
    InterfaceAmbiguousMethod = "interface.ambiguous-method",
    InterfaceDuplicateImpl = "interface.duplicate-impl",
    InterfaceDynViewMode = "interface.dyn-view-mode",
    InterfaceHeadMismatch = "interface.head-mismatch",
    InterfaceImplMisplaced = "interface.impl-misplaced",
    InterfaceImplParameters = "interface.impl-parameters",
    InterfaceImplTarget = "interface.impl-target",
    InterfaceImplVisibility = "interface.impl-visibility",
    InterfaceMethodClause = "interface.method-clause",
    InterfaceMethodCollision = "interface.method-collision",
    InterfaceMissingMethod = "interface.missing-method",
    InterfaceNotDynUsable = "interface.not-dyn-usable",
    InterfaceNotImplemented = "interface.not-implemented",
    InterfaceNotInterface = "interface.not-interface",
    InterfaceUnqualifiedCall = "interface.unqualified-call",
    InternalInvariant = "internal.invariant",
    ItemBodyOpener = "item.body-opener",
    ItemClauseOrder = "item.clause-order",
    ItemConstCycle = "item.const-cycle",
    ItemConstType = "item.const-type",
    ItemConstUntyped = "item.const-untyped",
    ItemDuplicate = "item.duplicate",
    ItemFailsMissing = "item.fails-missing",
    ItemFailsUnneeded = "item.fails-unneeded",
    ItemForeignForm = "item.foreign-form",
    ItemIndented = "item.indented",
    ItemLabelMismatch = "item.label-mismatch",
    ItemMethodMisplaced = "item.method-misplaced",
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
    ItemWaitMissing = "item.wait-missing",
    ItemWaitUnneeded = "item.wait-unneeded",
    KernelAnchorOutsideEntry = "kernel.anchor-outside-entry",
    KernelFloatUse = "kernel.float-use",
    KernelIrqCall = "kernel.irq-call",
    KernelIrqHead = "kernel.irq-head",
    KernelIrqLimit = "kernel.irq-limit",
    KernelIrqWait = "kernel.irq-wait",
    LanguageDowngrade = "language.downgrade",
    LanguageNewerDependency = "language.newer-dependency",
    LanguageUnsupported = "language.unsupported",
    LanguageVersionForm = "language.version-form",
    LayoutEndianness = "layout.endianness",
    LayoutFieldForm = "layout.field-form",
    LayoutFieldType = "layout.field-type",
    LayoutLiteral = "layout.literal",
    LayoutOutOfRange = "layout.out-of-range",
    LayoutOverlap = "layout.overlap",
    LayoutRepr = "layout.repr",
    LayoutTagValue = "layout.tag-value",
    LayoutUncovered = "layout.uncovered",
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
    LimitAllocates = "limit.allocates",
    LimitDuplicate = "limit.duplicate",
    LimitImpure = "limit.impure",
    LimitMayNotTerminate = "limit.may-not-terminate",
    LimitMayTrap = "limit.may-trap",
    LimitRedundant = "limit.redundant",
    LimitStackExceeded = "limit.stack-exceeded",
    LimitStackSize = "limit.stack-size",
    LimitStackUnbounded = "limit.stack-unbounded",
    ManifestAnyNotAlone = "manifest.any-not-alone",
    ManifestBadSource = "manifest.bad-source",
    ManifestDenySelf = "manifest.deny-self",
    ManifestDuplicate = "manifest.duplicate",
    ManifestMisplaced = "manifest.misplaced",
    ManifestMissing = "manifest.missing",
    ManifestMissingDirective = "manifest.missing-directive",
    ManifestReservedTarget = "manifest.reserved-target",
    ManifestUnknownTarget = "manifest.unknown-target",
    MergeConflict = "merge.conflict",
    MergeSemanticConflict = "merge.semantic-conflict",
    MergeUnresolved = "merge.unresolved",
    ModeCopyTaken = "mode.copy-taken",
    ModeImmutableBinding = "mode.immutable-binding",
    ModeMismatch = "mode.mismatch",
    ModeUnmarked = "mode.unmarked",
    ModuleCycle = "module.cycle",
    ModuleVariantMismatch = "module.variant-mismatch",
    ModuleVariantMissing = "module.variant-missing",
    ModuleVariantMixed = "module.variant-mixed",
    ModuleVariantOverlap = "module.variant-overlap",
    ModuleVariantUnmatched = "module.variant-unmatched",
    NameCase = "name.case",
    NameClash = "name.clash",
    NameColonForMember = "name.colon-for-member",
    NameCoreRedefined = "name.core-redefined",
    NameDotForPath = "name.dot-for-path",
    NameMoved = "name.moved",
    NameNewerRelease = "name.newer-release",
    NamePrefixMismatch = "name.prefix-mismatch",
    NamePrivate = "name.private",
    NamePrivateInInterface = "name.private-in-interface",
    NameRedundantPath = "name.redundant-path",
    NameReserved = "name.reserved",
    NameUnknown = "name.unknown",
    NirMalformed = "nir.malformed",
    NirVersionUnsupported = "nir.version-unsupported",
    NumConversionUndefined = "num.conversion-undefined",
    NumConversionValue = "num.conversion-value",
    NumFromBitsNotAnybits = "num.from-bits-not-anybits",
    NumSameTypeConversion = "num.same-type-conversion",
    NumSignedDivision = "num.signed-division",
    NumSignedShiftRight = "num.signed-shift-right",
    NumUnsignedNegation = "num.unsigned-negation",
    NumWidenApplies = "num.widen-applies",
    NumWidenLossy = "num.widen-lossy",
    OwnConflict = "own.conflict",
    OwnLinearDropped = "own.linear-dropped",
    OwnMoveNotOwned = "own.move-not-owned",
    OwnNotIndexable = "own.not-indexable",
    OwnNotIterable = "own.not-iterable",
    OwnProjReceiver = "own.proj-receiver",
    OwnProjSource = "own.proj-source",
    OwnReadOnlyView = "own.read-only-view",
    OwnTemporarySource = "own.temporary-source",
    OwnUseAfterMove = "own.use-after-move",
    OwnViewEscape = "own.view-escape",
    OwnWrappedPartUse = "own.wrapped-part-use",
    PatternConstKind = "pattern.const-kind",
    PatternDuplicateBinding = "pattern.duplicate-binding",
    PatternEmptyRange = "pattern.empty-range",
    PatternOpenEnumNoElse = "pattern.open-enum-no-else",
    PatternOpenStruct = "pattern.open-struct",
    PatternPathKind = "pattern.path-kind",
    PatternRangeKind = "pattern.range-kind",
    PatternRefutable = "pattern.refutable",
    PredicateDefaultUnproven = "predicate.default-unproven",
    PredicateExposedField = "predicate.exposed-field",
    PredicateOutsideWrite = "predicate.outside-write",
    ProofRefuted = "proof.refuted",
    ProofUnknown = "proof.unknown",
    PropCapabilityDerive = "prop.capability-derive",
    PropCopyIneligible = "prop.copy-ineligible",
    PropLinearArgument = "prop.linear-argument",
    PropLinearUndeclared = "prop.linear-undeclared",
    PropPromiseNotOpen = "prop.promise-not-open",
    PropPromiseUnmet = "prop.promise-unmet",
    PropPropertyImpl = "prop.property-impl",
    PropTokenArgument = "prop.token-argument",
    PropTokenWait = "prop.token-wait",
    PropViewArgument = "prop.view-argument",
    PropViewResult = "prop.view-result",
    PropViewUndeclared = "prop.view-undeclared",
    RegisterArrayAccess = "register.array-access",
    RegisterBankReserved = "register.bank-reserved",
    RegisterLiteral = "register.literal",
    RegisterMisaligned = "register.misaligned",
    RegisterNotReadable = "register.not-readable",
    RegisterNotWritable = "register.not-writable",
    RegisterOperand = "register.operand",
    RegisterOverlap = "register.overlap",
    RegisterPortWidth = "register.port-width",
    RegisterStride = "register.stride",
    RegisterType = "register.type",
    RegisterWindowType = "register.window-type",
    RegistryDuplicateKey = "registry.duplicate-key",
    RegistryDuplicateUnique = "registry.duplicate-unique",
    RegistryEntryType = "registry.entry-type",
    RegistryGrantNotAlias = "registry.grant-not-alias",
    RegistryKeyType = "registry.key-type",
    RegistryNotRegistry = "registry.not-registry",
    RegistryPlatformEntry = "registry.platform-entry",
    RegistryPlatformTarget = "registry.platform-target",
    RegistryUngranted = "registry.ungranted",
    RegistryUniqueRepeated = "registry.unique-repeated",
    RegistryUniqueType = "registry.unique-type",
    RegistryUnknownField = "registry.unknown-field",
    RetiredAmbiguousUse = "retired.ambiguous-use",
    RetiredBadRewrite = "retired.bad-rewrite",
    RetiredForeignPath = "retired.foreign-path",
    RetiredFormTaken = "retired.form-taken",
    RetiredForwardedUse = "retired.forwarded-use",
    RetiredNotPreserved = "retired.not-preserved",
    RetiredRewriteHidden = "retired.rewrite-hidden",
    SealBadPrincipal = "seal.bad-principal",
    SealSameAuthor = "seal.same-author",
    SealUnneeded = "seal.unneeded",
    StmtAssignTarget = "stmt.assign-target",
    StmtDiscardUnit = "stmt.discard-unit",
    StmtElseNotDiverging = "stmt.else-not-diverging",
    StmtIrrefutableElse = "stmt.irrefutable-else",
    StmtMissingReturn = "stmt.missing-return",
    StmtNoLoop = "stmt.no-loop",
    StmtReturnValue = "stmt.return-value",
    StmtShadowing = "stmt.shadowing",
    StmtUnknownLabel = "stmt.unknown-label",
    StmtUnreachable = "stmt.unreachable",
    StmtUnusedLabel = "stmt.unused-label",
    StmtUnusedResult = "stmt.unused-result",
    StmtWildcardBinding = "stmt.wildcard-binding",
    SuspendStackMissing = "suspend.stack-missing",
    SuspendTokenAcrossAwait = "suspend.token-across-await",
    SyntaxForeign = "syntax.foreign",
    SyntaxMismatchedDelimiter = "syntax.mismatched-delimiter",
    SyntaxUnclosedDelimiter = "syntax.unclosed-delimiter",
    SyntaxUnexpectedToken = "syntax.unexpected-token",
    TargetDepProgram = "target.dep-program",
    TargetDependencyUncovered = "target.dependency-uncovered",
    TargetEntryHead = "target.entry-head",
    TargetEntryMissing = "target.entry-missing",
    TargetLinkFailed = "target.link-failed",
    TargetProgramTargets = "target.program-targets",
    TaskCaptureNotSend = "task.capture-not-send",
    TaskLendLocal = "task.lend-local",
    TaskLendOutsideScope = "task.lend-outside-scope",
    TaskTaskFnCall = "task.task-fn-call",
    TaskUnscopedFailure = "task.unscoped-failure",
    TaskWhenDuplicate = "task.when-duplicate",
    TaskWhenEscape = "task.when-escape",
    TaskWhenNotCown = "task.when-not-cown",
    TaskWhenTake = "task.when-take",
    TaskWhenWait = "task.when-wait",
    TestDbg = "test.dbg",
    TestGenerateManualImpl = "test.generate-manual-impl",
    TestGenerateType = "test.generate-type",
    TestGeneratorGap = "test.generator-gap",
    TestParameterForm = "test.parameter-form",
    TestParameterType = "test.parameter-type",
    TestReplayToken = "test.replay-token",
    TestTargetKind = "test.target-kind",
    TestTargetUnreachable = "test.target-unreachable",
    TestTestOnlyItem = "test.test-only-item",
    TextArgumentDuplicate = "text.argument-duplicate",
    TextArgumentUnused = "text.argument-unused",
    TextBareLen = "text.bare-len",
    TextHoleUnbound = "text.hole-unbound",
    TextIntegerIndex = "text.integer-index",
    TextTemplateMalformed = "text.template-malformed",
    TextTemplateNotLiteral = "text.template-not-literal",
    TextTemplatePosition = "text.template-position",
    ThresholdDepth = "threshold.depth",
    ThresholdEmptyReason = "threshold.empty-reason",
    ThresholdLength = "threshold.length",
    ThresholdUnusedWaiver = "threshold.unused-waiver",
    TodoDisputed = "todo.disputed",
    TodoEmptyReason = "todo.empty-reason",
    TodoHole = "todo.hole",
    TodoMissingSpec = "todo.missing-spec",
    TodoUntyped = "todo.untyped",
    ToolNoWorkspace = "tool.no-workspace",
    ToolUnknownScope = "tool.unknown-scope",
    ToolUsage = "tool.usage",
    TrustUnsealed = "trust.unsealed",
    TrustedBlockValue = "trusted.block-value",
    TrustedConventionTarget = "trusted.convention-target",
    TrustedConventionUnknown = "trusted.convention-unknown",
    TrustedEmptyPremise = "trusted.empty-premise",
    TrustedForeignSignature = "trusted.foreign-signature",
    TrustedNested = "trusted.nested",
    TrustedPrimitiveOutside = "trusted.primitive-outside",
    TypeClosedHiddenField = "type.closed-hidden-field",
    TypeDuplicateLabel = "type.duplicate-label",
    TypeDuplicateMember = "type.duplicate-member",
    TypeHiddenField = "type.hidden-field",
    TypeLiteralRange = "type.literal-range",
    TypeLiteralUntyped = "type.literal-untyped",
    TypeMismatch = "type.mismatch",
    TypeMissingField = "type.missing-field",
    TypeNotAType = "type.not-a-type",
    TypeOpenConstruction = "type.open-construction",
    TypePayloadShape = "type.payload-shape",
    TypeQualifiedCoreVariant = "type.qualified-core-variant",
    TypeRecursive = "type.recursive",
    TypeUnlabelledField = "type.unlabelled-field",
    TypeUnlabelledPayload = "type.unlabelled-payload",
    UntrustedInspectOutsideValidator = "untrusted.inspect-outside-validator",
    UntrustedValidatorInterface = "untrusted.validator-interface",
    UntrustedValidatorNoInput = "untrusted.validator-no-input",
    UntrustedWrappedAlias = "untrusted.wrapped-alias",
    UnusedDependency = "unused.dependency",
    UnusedField = "unused.field",
    UnusedGrant = "unused.grant",
    UnusedItem = "unused.item",
    UnusedLocal = "unused.local",
    UnusedTombstone = "unused.tombstone",
    VersionBreakingTarget = "version.breaking-target",
    VersionParentMissing = "version.parent-missing",
    VersionPathDependency = "version.path-dependency",
    VersionStoreNameTaken = "version.store-name-taken",
}

impl Code {
    pub fn class(self) -> Class {
        match self {
            Code::InternalInvariant => Class::Internal,
            Code::ApiStale
            | Code::ApiWeakeningPending
            | Code::DocStale
            | Code::RetiredForwardedUse
            | Code::SealSameAuthor
            | Code::TestDbg
            | Code::TestGeneratorGap
            | Code::ThresholdDepth
            | Code::ThresholdLength
            | Code::TodoDisputed
            | Code::TodoHole
            | Code::TodoMissingSpec
            | Code::TrustUnsealed
            | Code::UnusedDependency
            | Code::UnusedField
            | Code::UnusedGrant
            | Code::UnusedItem
            | Code::UnusedLocal
            | Code::UnusedTombstone => Class::Incompleteness,
            _ => Class::Error,
        }
    }
}

/// The role of a reason in a because-chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Its line declares the subject the reported construct is checked against.
    Declared,
    /// Its line binds or creates the subject.
    Bound,
    /// Its line starts a hold freezing the subject.
    Frozen,
    /// Its line starts a hold holding the subject.
    Held,
    /// Its line moves the subject.
    Moved,
    /// Its line uses the subject, keeping a hold or a value live.
    Used,
    /// Its line accesses the subject, written with its mode.
    Accessed,
    /// Its line calls the subject, through which a fact reaches the reported line.
    Called,
    /// Its line is the site of a fact; the subject is the operation.
    Raised,
    /// The command making the change the owning rule requires, on no line.
    Command,
    /// A value the owning rule names, on no line.
    Note,
}

impl Role {
    pub fn word(self) -> &'static str {
        match self {
            Role::Declared => "declared",
            Role::Bound => "bound",
            Role::Frozen => "frozen",
            Role::Held => "held",
            Role::Moved => "moved",
            Role::Used => "used",
            Role::Accessed => "accessed",
            Role::Called => "called",
            Role::Raised => "raised",
            Role::Command => "command",
            Role::Note => "note",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason {
    pub role: Role,
    pub subject: String,
    /// The workspace file and line the reason lies on, when it lies in source.
    pub at: Option<(String, u32)>,
}

impl Reason {
    pub fn note(subject: &str) -> Reason {
        Reason {
            role: Role::Note,
            subject: subject.to_owned(),
            at: None,
        }
    }
}

/// A replacement of the bytes `start..end` of a source file, an empty run inserting the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replacement {
    pub start: u32,
    pub end: u32,
    pub text: String,
}

/// An exact fix: replacements in the workspace's source files, by path, none overlapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix(pub Vec<(String, Replacement)>);

impl Fix {
    /// The fix making this one's replacements and another's.
    pub fn and(mut self, other: Fix) -> Fix {
        self.0.extend(other.0);
        self
    }
}

/// Which of the fixes a rule names a diagnostic offers, among those passing the in-memory
/// re-check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Offer {
    /// Each.
    #[default]
    Each,
    /// The first, the others being its alternatives.
    First,
    /// The one, when exactly one passes.
    Unique,
}

/// One reported root cause. The quote and the item are derived from the location when printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub path: Option<String>,
    pub line: Option<u32>,
    /// The canonical path of the item it is reported on, when the checker reports it.
    pub item: Option<String>,
    pub expected: Option<String>,
    pub found: Option<String>,
    pub because: Vec<Reason>,
    /// The bytes of the reported construct in its file.
    pub span: Option<(u32, u32)>,
    /// The bytes of the construct its rule judges, when wider than the reported construct.
    pub judged: Option<(u32, u32)>,
    /// For a syntax failure, the first byte of the token at which the parse failed.
    pub failing: Option<u32>,
    /// The fixes its rule names, or once re-checked, those it offers.
    pub fixes: Vec<Fix>,
    pub offer: Offer,
    /// The output of an external program it quotes: the program's name and the lines.
    pub envelope: Option<(String, Vec<String>)>,
}

impl Diagnostic {
    pub fn new(code: Code) -> Diagnostic {
        Diagnostic {
            code,
            path: None,
            line: None,
            item: None,
            expected: None,
            found: None,
            because: Vec::new(),
            span: None,
            judged: None,
            failing: None,
            fixes: Vec::new(),
            offer: Offer::Each,
            envelope: None,
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
