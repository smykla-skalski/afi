//! How a run is asked to report itself.
//!
//! Its own file because the value has three readers - the flag, the variable, and
//! the config file - and only the last of them may refuse an unrecognized one.

/// How to report the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SummaryFormat {
    /// Print nothing extra. The default, so existing behaviour is unchanged.
    #[default]
    None,
    /// One JSON object on stdout.
    Json,
}

impl SummaryFormat {
    /// The values [`Self::parse`] accepts, for a caller that has to refuse
    /// anything else - the config file, where an unrecognized value is an error.
    pub const NAMES: [&str; 2] = ["json", "none"];

    /// Parse `--summary` / `AFI_SUMMARY`. An unrecognized value is `None` rather
    /// than an error: a typo must not lose a completed run's output.
    #[must_use]
    pub fn from_value(raw: Option<&str>) -> Self {
        Self::parse(raw.unwrap_or_default()).unwrap_or(Self::None)
    }

    /// The format `raw` names, or `None` when it names none.
    ///
    /// Split out of [`Self::from_value`] so a caller that must refuse an
    /// unrecognized value can tell one from `none` - the flag and the variable
    /// cannot, by design.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "json" => Some(Self::Json),
            "none" | "" => Some(Self::None),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_json(self) -> bool {
        self == Self::Json
    }
}
