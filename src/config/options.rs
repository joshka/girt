use super::IncludePlacement;

/// Explicit include evaluation choices, independent of source discovery and trust.
///
/// Defaults preserve [`super::Config::resolve`]. These choices never discover paths or read the
/// process environment; interpolation uses the caller's [`super::IncludeContext`].
///
/// Choose nondefault evaluation only to meet an explicit consumer contract. Placement controls
/// effective output order; it does not change source validation order or establish the
/// trustworthiness of included files.
///
/// ```
/// use girt::Config;
/// use girt::config::{ConfigInputs, ResolveOptions, UnresolvedIncludePath};
/// let inputs = ConfigInputs::default()
///     .with_command(Some(Config::parse(b"[include]\npath = ~/unmapped\n")?));
/// assert!(Config::resolve(&inputs).is_err());
/// let options =
///     ResolveOptions::default().with_unresolved_paths(UnresolvedIncludePath::Skip);
/// let resolved = Config::resolve_with_options(&inputs, options)?;
/// assert!(resolved.value("include", None, "path").is_some()); // Directive retained, expansion skipped.
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ResolveOptions {
    /// Where successfully included blocks appear in the resulting snapshot.
    pub placement: IncludePlacement,
    /// Which remote URLs conditional includes can observe.
    pub conditions: IncludeConditionVisibility,
    /// How include section names are recognized.
    pub directive_case: IncludeDirectiveCase,
    /// Treatment of unavailable path interpolation context.
    pub unresolved_paths: UnresolvedIncludePath,
}

impl ResolveOptions {
    /// Sets [`Self::placement`].
    #[must_use]
    pub fn with_placement(mut self, placement: IncludePlacement) -> Self {
        self.placement = placement;
        self
    }

    /// Sets [`Self::conditions`].
    #[must_use]
    pub fn with_conditions(mut self, conditions: IncludeConditionVisibility) -> Self {
        self.conditions = conditions;
        self
    }

    /// Sets [`Self::directive_case`].
    #[must_use]
    pub fn with_directive_case(mut self, directive_case: IncludeDirectiveCase) -> Self {
        self.directive_case = directive_case;
        self
    }

    /// Sets [`Self::unresolved_paths`].
    #[must_use]
    pub fn with_unresolved_paths(mut self, unresolved_paths: UnresolvedIncludePath) -> Self {
        self.unresolved_paths = unresolved_paths;
        self
    }
}

/// Remote URL visibility while evaluating `hasconfig:remote.*.url:` conditions.
///
/// Keep [`Self::AllInputs`] for ordinary Git-style resolution. [`Self::RootSnapshot`] serves
/// consumers requiring root-local frozen URL visibility; it can select different include subtrees,
/// so choose it as an explicit compatibility policy. Choose it when each source must be evaluated
/// independently of remote URLs supplied by other scopes, rather than allowing a later command
/// override to select an earlier file's conditional include. It also permits selected conditional
/// children to add URLs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum IncludeConditionVisibility {
    /// Prescan all roots and include descendants. A hasconfig descendant may not declare URLs.
    #[default]
    AllInputs,
    /// Seed each root with all its own URLs, regardless of position. Each direct include subtree
    /// sees that root's frozen URL view throughout recursion. Only after the complete subtree
    /// returns do its recursively included URLs become visible to later root directives.
    /// Other roots are invisible; unmatched conditional children are not read or validated.
    /// Selected hasconfig children may declare URLs. Evaluation remains forward regardless of
    /// output placement.
    RootSnapshot,
}

/// Case matching for include directive section names; `path` keys always ignore ASCII case.
///
/// Keep [`Self::Insensitive`] matching for ordinary resolution. [`Self::Canonical`] is an explicit
/// consumer policy that can leave differently cased include sections inert; it does not change
/// ordinary variable-name matching. Choose it only when adapting a consumer whose decoded directive
/// vocabulary distinguishes exact `include`/`includeIf` spellings; differently cased sections
/// then remain ordinary data. Use [`Self::Insensitive`] for files shared with ordinary Git
/// resolution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum IncludeDirectiveCase {
    /// Recognize include section names ignoring ASCII case.
    #[default]
    Insensitive,
    /// Recognize only exact `include` and `includeIf` section names.
    Canonical,
}

/// Treatment of include paths whose requested interpolation context was not supplied.
///
/// Use [`Self::Error`] when configuration must be complete. [`Self::Skip`] permits deliberate
/// partial interpretation without home/prefix context; retained directives still do not become
/// resolved sources.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum UnresolvedIncludePath {
    /// Report a missing home, named home or installation prefix as an input error.
    #[default]
    Error,
    /// Retain the directive but skip its expansion when interpolation context is absent.
    /// Does not ignore path encoding, source I/O, parsing, cycle or resource-limit failures.
    Skip,
}
