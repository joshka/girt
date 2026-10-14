use super::IncludePlacement;

/// Explicit include evaluation choices, independent of source discovery and trust.
///
/// Defaults preserve [`super::Config::resolve`]. These choices never discover paths or read the
/// process environment; interpolation uses the caller's [`super::IncludeContext`].
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
