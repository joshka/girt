//! Typed diagnostic catalogue from the installed Git 2.56 fsck manual, not implementation source.

/// Severity in Git's documented diagnostic catalogue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Severity {
    /// Structural corruption that cannot be suppressed by policy.
    Fatal,
    /// Makes the integrity check fail.
    Error,
    /// Reported without failing the check.
    Warning,
    /// Historical compatibility observation; Git reports these as warnings.
    Info,
    /// Suppressed by default or explicit policy.
    Ignore,
}

/// Git fsck message IDs, plus girt's legacy aggregate parse diagnostics.
///
/// Recognition does not imply every diagnostic is emitted by the current checker.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum MessageId {
    /// The `badDate` diagnostic.
    BadDate,
    /// The `badDateOverflow` diagnostic.
    BadDateOverflow,
    /// The `badEmail` diagnostic.
    BadEmail,
    /// The `badFilemode` diagnostic.
    BadFilemode,
    /// The `badGpgsig` diagnostic.
    BadGpgsig,
    /// The `badHeadTarget` diagnostic.
    BadHeadTarget,
    /// The `badHeaderContinuation` diagnostic.
    BadHeaderContinuation,
    /// The `badName` diagnostic.
    BadName,
    /// The `badObjectSha1` diagnostic.
    BadObjectSha1,
    /// The `badPackedRefEntry` diagnostic.
    BadPackedRefEntry,
    /// The `badPackedRefHeader` diagnostic.
    BadPackedRefHeader,
    /// The `badParentSha1` diagnostic.
    BadParentSha1,
    /// The `badRefContent` diagnostic.
    BadRefContent,
    /// The `badRefFiletype` diagnostic.
    BadRefFiletype,
    /// The `badRefName` diagnostic.
    BadRefName,
    /// The `badRefOid` diagnostic.
    BadRefOid,
    /// The `badReferentName` diagnostic.
    BadReferentName,
    /// The `badReftableTableName` diagnostic.
    BadReftableTableName,
    /// The `badTagName` diagnostic.
    BadTagName,
    /// The `badTimezone` diagnostic.
    BadTimezone,
    /// The `badTree` diagnostic.
    BadTree,
    /// The `badTreeSha1` diagnostic.
    BadTreeSha1,
    /// The `badType` diagnostic.
    BadType,
    /// The `duplicateEntries` diagnostic.
    DuplicateEntries,
    /// The `emptyName` diagnostic.
    EmptyName,
    /// The `emptyPackedRefsFile` diagnostic.
    EmptyPackedRefsFile,
    /// The `extraHeaderEntry` diagnostic.
    ExtraHeaderEntry,
    /// The `fullPathname` diagnostic.
    FullPathname,
    /// The `gitattributesBlob` diagnostic.
    GitattributesBlob,
    /// The `gitattributesLarge` diagnostic.
    GitattributesLarge,
    /// The `gitattributesLineLength` diagnostic.
    GitattributesLineLength,
    /// The `gitattributesMissing` diagnostic.
    GitattributesMissing,
    /// The `gitattributesSymlink` diagnostic.
    GitattributesSymlink,
    /// The `gitignoreSymlink` diagnostic.
    GitignoreSymlink,
    /// The `gitmodulesBlob` diagnostic.
    GitmodulesBlob,
    /// The `gitmodulesLarge` diagnostic.
    GitmodulesLarge,
    /// The `gitmodulesMissing` diagnostic.
    GitmodulesMissing,
    /// The `gitmodulesName` diagnostic.
    GitmodulesName,
    /// The `gitmodulesParse` diagnostic.
    GitmodulesParse,
    /// The `gitmodulesPath` diagnostic.
    GitmodulesPath,
    /// The `gitmodulesSymlink` diagnostic.
    GitmodulesSymlink,
    /// The `gitmodulesUpdate` diagnostic.
    GitmodulesUpdate,
    /// The `gitmodulesUrl` diagnostic.
    GitmodulesUrl,
    /// The `hasDot` diagnostic.
    HasDot,
    /// The `hasDotdot` diagnostic.
    HasDotdot,
    /// The `hasDotgit` diagnostic.
    HasDotgit,
    /// The `largePathname` diagnostic.
    LargePathname,
    /// The `mailmapSymlink` diagnostic.
    MailmapSymlink,
    /// The `missingAuthor` diagnostic.
    MissingAuthor,
    /// The `missingCommitter` diagnostic.
    MissingCommitter,
    /// The `missingEmail` diagnostic.
    MissingEmail,
    /// The `missingNameBeforeEmail` diagnostic.
    MissingNameBeforeEmail,
    /// The `missingObject` diagnostic.
    MissingObject,
    /// The `missingSpaceBeforeDate` diagnostic.
    MissingSpaceBeforeDate,
    /// The `missingSpaceBeforeEmail` diagnostic.
    MissingSpaceBeforeEmail,
    /// The `missingTag` diagnostic.
    MissingTag,
    /// The `missingTagEntry` diagnostic.
    MissingTagEntry,
    /// The `missingTaggerEntry` diagnostic.
    MissingTaggerEntry,
    /// The `missingTree` diagnostic.
    MissingTree,
    /// The `missingType` diagnostic.
    MissingType,
    /// The `missingTypeEntry` diagnostic.
    MissingTypeEntry,
    /// The `multipleAuthors` diagnostic.
    MultipleAuthors,
    /// The `nulInCommit` diagnostic.
    NulInCommit,
    /// The `nulInHeader` diagnostic.
    NulInHeader,
    /// The `nullSha1` diagnostic.
    NullSha1,
    /// The `packedRefEntryNotTerminated` diagnostic.
    PackedRefEntryNotTerminated,
    /// The `packedRefUnsorted` diagnostic.
    PackedRefUnsorted,
    /// The `refMissingNewline` diagnostic.
    RefMissingNewline,
    /// The `symlinkRef` diagnostic.
    SymlinkRef,
    /// The `symrefTargetIsNotARef` diagnostic.
    SymrefTargetIsNotARef,
    /// The `trailingRefContent` diagnostic.
    TrailingRefContent,
    /// The `treeNotSorted` diagnostic.
    TreeNotSorted,
    /// The `unknownType` diagnostic.
    UnknownType,
    /// The `unterminatedHeader` diagnostic.
    UnterminatedHeader,
    /// The `zeroPaddedDate` diagnostic.
    ZeroPaddedDate,
    /// The `zeroPaddedFilemode` diagnostic.
    ZeroPaddedFilemode,
    /// The `badCommit` diagnostic.
    BadCommit,
    /// The `badTagObject` diagnostic.
    BadTagObject,
}

impl MessageId {
    /// Every recognized diagnostic, in catalogue order.
    pub const ALL: &'static [Self] = &[
        Self::BadDate,
        Self::BadDateOverflow,
        Self::BadEmail,
        Self::BadFilemode,
        Self::BadGpgsig,
        Self::BadHeadTarget,
        Self::BadHeaderContinuation,
        Self::BadName,
        Self::BadObjectSha1,
        Self::BadPackedRefEntry,
        Self::BadPackedRefHeader,
        Self::BadParentSha1,
        Self::BadRefContent,
        Self::BadRefFiletype,
        Self::BadRefName,
        Self::BadRefOid,
        Self::BadReferentName,
        Self::BadReftableTableName,
        Self::BadTagName,
        Self::BadTimezone,
        Self::BadTree,
        Self::BadTreeSha1,
        Self::BadType,
        Self::DuplicateEntries,
        Self::EmptyName,
        Self::EmptyPackedRefsFile,
        Self::ExtraHeaderEntry,
        Self::FullPathname,
        Self::GitattributesBlob,
        Self::GitattributesLarge,
        Self::GitattributesLineLength,
        Self::GitattributesMissing,
        Self::GitattributesSymlink,
        Self::GitignoreSymlink,
        Self::GitmodulesBlob,
        Self::GitmodulesLarge,
        Self::GitmodulesMissing,
        Self::GitmodulesName,
        Self::GitmodulesParse,
        Self::GitmodulesPath,
        Self::GitmodulesSymlink,
        Self::GitmodulesUpdate,
        Self::GitmodulesUrl,
        Self::HasDot,
        Self::HasDotdot,
        Self::HasDotgit,
        Self::LargePathname,
        Self::MailmapSymlink,
        Self::MissingAuthor,
        Self::MissingCommitter,
        Self::MissingEmail,
        Self::MissingNameBeforeEmail,
        Self::MissingObject,
        Self::MissingSpaceBeforeDate,
        Self::MissingSpaceBeforeEmail,
        Self::MissingTag,
        Self::MissingTagEntry,
        Self::MissingTaggerEntry,
        Self::MissingTree,
        Self::MissingType,
        Self::MissingTypeEntry,
        Self::MultipleAuthors,
        Self::NulInCommit,
        Self::NulInHeader,
        Self::NullSha1,
        Self::PackedRefEntryNotTerminated,
        Self::PackedRefUnsorted,
        Self::RefMissingNewline,
        Self::SymlinkRef,
        Self::SymrefTargetIsNotARef,
        Self::TrailingRefContent,
        Self::TreeNotSorted,
        Self::UnknownType,
        Self::UnterminatedHeader,
        Self::ZeroPaddedDate,
        Self::ZeroPaddedFilemode,
        Self::BadCommit,
        Self::BadTagObject,
    ];

    /// Git's diagnostic spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadDate => "badDate",
            Self::BadDateOverflow => "badDateOverflow",
            Self::BadEmail => "badEmail",
            Self::BadFilemode => "badFilemode",
            Self::BadGpgsig => "badGpgsig",
            Self::BadHeadTarget => "badHeadTarget",
            Self::BadHeaderContinuation => "badHeaderContinuation",
            Self::BadName => "badName",
            Self::BadObjectSha1 => "badObjectSha1",
            Self::BadPackedRefEntry => "badPackedRefEntry",
            Self::BadPackedRefHeader => "badPackedRefHeader",
            Self::BadParentSha1 => "badParentSha1",
            Self::BadRefContent => "badRefContent",
            Self::BadRefFiletype => "badRefFiletype",
            Self::BadRefName => "badRefName",
            Self::BadRefOid => "badRefOid",
            Self::BadReferentName => "badReferentName",
            Self::BadReftableTableName => "badReftableTableName",
            Self::BadTagName => "badTagName",
            Self::BadTimezone => "badTimezone",
            Self::BadTree => "badTree",
            Self::BadTreeSha1 => "badTreeSha1",
            Self::BadType => "badType",
            Self::DuplicateEntries => "duplicateEntries",
            Self::EmptyName => "emptyName",
            Self::EmptyPackedRefsFile => "emptyPackedRefsFile",
            Self::ExtraHeaderEntry => "extraHeaderEntry",
            Self::FullPathname => "fullPathname",
            Self::GitattributesBlob => "gitattributesBlob",
            Self::GitattributesLarge => "gitattributesLarge",
            Self::GitattributesLineLength => "gitattributesLineLength",
            Self::GitattributesMissing => "gitattributesMissing",
            Self::GitattributesSymlink => "gitattributesSymlink",
            Self::GitignoreSymlink => "gitignoreSymlink",
            Self::GitmodulesBlob => "gitmodulesBlob",
            Self::GitmodulesLarge => "gitmodulesLarge",
            Self::GitmodulesMissing => "gitmodulesMissing",
            Self::GitmodulesName => "gitmodulesName",
            Self::GitmodulesParse => "gitmodulesParse",
            Self::GitmodulesPath => "gitmodulesPath",
            Self::GitmodulesSymlink => "gitmodulesSymlink",
            Self::GitmodulesUpdate => "gitmodulesUpdate",
            Self::GitmodulesUrl => "gitmodulesUrl",
            Self::HasDot => "hasDot",
            Self::HasDotdot => "hasDotdot",
            Self::HasDotgit => "hasDotgit",
            Self::LargePathname => "largePathname",
            Self::MailmapSymlink => "mailmapSymlink",
            Self::MissingAuthor => "missingAuthor",
            Self::MissingCommitter => "missingCommitter",
            Self::MissingEmail => "missingEmail",
            Self::MissingNameBeforeEmail => "missingNameBeforeEmail",
            Self::MissingObject => "missingObject",
            Self::MissingSpaceBeforeDate => "missingSpaceBeforeDate",
            Self::MissingSpaceBeforeEmail => "missingSpaceBeforeEmail",
            Self::MissingTag => "missingTag",
            Self::MissingTagEntry => "missingTagEntry",
            Self::MissingTaggerEntry => "missingTaggerEntry",
            Self::MissingTree => "missingTree",
            Self::MissingType => "missingType",
            Self::MissingTypeEntry => "missingTypeEntry",
            Self::MultipleAuthors => "multipleAuthors",
            Self::NulInCommit => "nulInCommit",
            Self::NulInHeader => "nulInHeader",
            Self::NullSha1 => "nullSha1",
            Self::PackedRefEntryNotTerminated => "packedRefEntryNotTerminated",
            Self::PackedRefUnsorted => "packedRefUnsorted",
            Self::RefMissingNewline => "refMissingNewline",
            Self::SymlinkRef => "symlinkRef",
            Self::SymrefTargetIsNotARef => "symrefTargetIsNotARef",
            Self::TrailingRefContent => "trailingRefContent",
            Self::TreeNotSorted => "treeNotSorted",
            Self::UnknownType => "unknownType",
            Self::UnterminatedHeader => "unterminatedHeader",
            Self::ZeroPaddedDate => "zeroPaddedDate",
            Self::ZeroPaddedFilemode => "zeroPaddedFilemode",
            Self::BadCommit => "badCommit",
            Self::BadTagObject => "badTagObject",
        }
    }

    /// Looks up a case-insensitive configuration message ID.
    pub fn from_config_name(name: &[u8]) -> Option<Self> {
        Self::ALL.iter().copied().find(|id| {
            !matches!(id, Self::BadCommit | Self::BadTagObject)
                && id.as_str().as_bytes().eq_ignore_ascii_case(name)
        })
    }

    /// Severity listed in Git's fsck manual, before strict mode and reporting promotion.
    pub fn default_severity(self) -> Severity {
        match self {
            Self::BadDate => Severity::Error,
            Self::BadDateOverflow => Severity::Error,
            Self::BadEmail => Severity::Error,
            Self::BadFilemode => Severity::Info,
            Self::BadGpgsig => Severity::Error,
            Self::BadHeadTarget => Severity::Error,
            Self::BadHeaderContinuation => Severity::Error,
            Self::BadName => Severity::Error,
            Self::BadObjectSha1 => Severity::Error,
            Self::BadPackedRefEntry => Severity::Error,
            Self::BadPackedRefHeader => Severity::Error,
            Self::BadParentSha1 => Severity::Error,
            Self::BadRefContent => Severity::Error,
            Self::BadRefFiletype => Severity::Error,
            Self::BadRefName => Severity::Error,
            Self::BadRefOid => Severity::Error,
            Self::BadReferentName => Severity::Error,
            Self::BadReftableTableName => Severity::Warning,
            Self::BadTagName => Severity::Info,
            Self::BadTimezone => Severity::Error,
            Self::BadTree => Severity::Error,
            Self::BadTreeSha1 => Severity::Error,
            Self::BadType => Severity::Error,
            Self::DuplicateEntries => Severity::Error,
            Self::EmptyName => Severity::Warning,
            Self::EmptyPackedRefsFile => Severity::Info,
            Self::ExtraHeaderEntry => Severity::Ignore,
            Self::FullPathname => Severity::Warning,
            Self::GitattributesBlob => Severity::Error,
            Self::GitattributesLarge => Severity::Error,
            Self::GitattributesLineLength => Severity::Error,
            Self::GitattributesMissing => Severity::Error,
            Self::GitattributesSymlink => Severity::Info,
            Self::GitignoreSymlink => Severity::Info,
            Self::GitmodulesBlob => Severity::Error,
            Self::GitmodulesLarge => Severity::Error,
            Self::GitmodulesMissing => Severity::Error,
            Self::GitmodulesName => Severity::Error,
            Self::GitmodulesParse => Severity::Info,
            Self::GitmodulesPath => Severity::Error,
            Self::GitmodulesSymlink => Severity::Error,
            Self::GitmodulesUpdate => Severity::Error,
            Self::GitmodulesUrl => Severity::Error,
            Self::HasDot => Severity::Warning,
            Self::HasDotdot => Severity::Warning,
            Self::HasDotgit => Severity::Warning,
            Self::LargePathname => Severity::Warning,
            Self::MailmapSymlink => Severity::Info,
            Self::MissingAuthor => Severity::Error,
            Self::MissingCommitter => Severity::Error,
            Self::MissingEmail => Severity::Error,
            Self::MissingNameBeforeEmail => Severity::Error,
            Self::MissingObject => Severity::Error,
            Self::MissingSpaceBeforeDate => Severity::Error,
            Self::MissingSpaceBeforeEmail => Severity::Error,
            Self::MissingTag => Severity::Error,
            Self::MissingTagEntry => Severity::Error,
            Self::MissingTaggerEntry => Severity::Info,
            Self::MissingTree => Severity::Error,
            Self::MissingType => Severity::Error,
            Self::MissingTypeEntry => Severity::Error,
            Self::MultipleAuthors => Severity::Error,
            Self::NulInCommit => Severity::Warning,
            Self::NulInHeader => Severity::Fatal,
            Self::NullSha1 => Severity::Warning,
            Self::PackedRefEntryNotTerminated => Severity::Error,
            Self::PackedRefUnsorted => Severity::Error,
            Self::RefMissingNewline => Severity::Info,
            Self::SymlinkRef => Severity::Info,
            Self::SymrefTargetIsNotARef => Severity::Info,
            Self::TrailingRefContent => Severity::Info,
            Self::TreeNotSorted => Severity::Error,
            Self::UnknownType => Severity::Error,
            Self::UnterminatedHeader => Severity::Fatal,
            Self::ZeroPaddedDate => Severity::Error,
            Self::ZeroPaddedFilemode => Severity::Warning,
            Self::BadCommit => Severity::Error,
            Self::BadTagObject => Severity::Error,
        }
    }

    /// Effective reporting severity. Strict mode promotes warnings; informational diagnostics
    /// are reported as warnings in both modes, matching Git's fsck CLI.
    pub fn severity(self, strict: bool) -> Severity {
        match self.default_severity() {
            Severity::Warning if strict => Severity::Error,
            Severity::Info => Severity::Warning,
            severity => severity,
        }
    }
}
