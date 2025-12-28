#[derive(Clone, Debug)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[cfg_attr(feature = "serde", serde(tag = "event"))]
pub enum Event {
    DiscoverStart(DiscoverStart),
    DiscoverCase(DiscoverCase),
    DiscoverComplete(DiscoverComplete),
    RunStart(RunStart),
    CaseStart(CaseStart),
    CaseMessage(CaseMessage),
    CaseComplete(CaseComplete),
    RunComplete(RunComplete),
}

impl Event {
    #[cfg(feature = "json")]
    pub fn to_jsonline(&self) -> String {
        match self {
            Self::DiscoverStart(event) => event.to_jsonline(),
            Self::DiscoverCase(event) => event.to_jsonline(),
            Self::DiscoverComplete(event) => event.to_jsonline(),
            Self::RunStart(event) => event.to_jsonline(),
            Self::CaseStart(event) => event.to_jsonline(),
            Self::CaseMessage(event) => event.to_jsonline(),
            Self::CaseComplete(event) => event.to_jsonline(),
            Self::RunComplete(event) => event.to_jsonline(),
        }
    }
}

impl From<DiscoverStart> for Event {
    fn from(inner: DiscoverStart) -> Self {
        Self::DiscoverStart(inner)
    }
}

impl From<DiscoverCase> for Event {
    fn from(inner: DiscoverCase) -> Self {
        Self::DiscoverCase(inner)
    }
}

impl From<DiscoverComplete> for Event {
    fn from(inner: DiscoverComplete) -> Self {
        Self::DiscoverComplete(inner)
    }
}

impl From<RunStart> for Event {
    fn from(inner: RunStart) -> Self {
        Self::RunStart(inner)
    }
}

impl From<CaseStart> for Event {
    fn from(inner: CaseStart) -> Self {
        Self::CaseStart(inner)
    }
}

impl From<CaseMessage> for Event {
    fn from(inner: CaseMessage) -> Self {
        Self::CaseMessage(inner)
    }
}

impl From<CaseComplete> for Event {
    fn from(inner: CaseComplete) -> Self {
        Self::CaseComplete(inner)
    }
}

impl From<RunComplete> for Event {
    fn from(inner: RunComplete) -> Self {
        Self::RunComplete(inner)
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub struct DiscoverStart {
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub elapsed_s: Option<Elapsed>,
}

impl DiscoverStart {
    #[cfg(feature = "json")]
    pub fn to_jsonline(&self) -> String {
        use json_write::JsonWrite as _;

        let mut buffer = String::new();
        buffer.open_object().unwrap();

        buffer.key("event").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value("discover_start").unwrap();

        if let Some(elapsed_s) = self.elapsed_s {
            buffer.val_sep().unwrap();
            buffer.key("elapsed_s").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(String::from(elapsed_s)).unwrap();
        }

        buffer.close_object().unwrap();

        buffer
    }
}

/// A test case was found
///
/// The order these are returned in is unspecified and is unrelated to the order they are run in.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub struct DiscoverCase {
    pub name: String,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "RunMode::is_default")
    )]
    pub mode: RunMode,
    /// Whether selected to be run by the user
    #[cfg_attr(
        feature = "serde",
        serde(default = "true_default", skip_serializing_if = "is_true")
    )]
    pub selected: bool,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub elapsed_s: Option<Elapsed>,
}

impl DiscoverCase {
    #[cfg(feature = "json")]
    pub fn to_jsonline(&self) -> String {
        use json_write::JsonWrite as _;

        let mut buffer = String::new();
        buffer.open_object().unwrap();

        buffer.key("event").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value("discover_case").unwrap();

        buffer.val_sep().unwrap();
        buffer.key("name").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value(&self.name).unwrap();

        if !self.mode.is_default() {
            buffer.val_sep().unwrap();
            buffer.key("mode").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(self.mode.as_str()).unwrap();
        }

        if !self.selected {
            buffer.val_sep().unwrap();
            buffer.key("selected").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(self.selected).unwrap();
        }

        if let Some(elapsed_s) = self.elapsed_s {
            buffer.val_sep().unwrap();
            buffer.key("elapsed_s").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(String::from(elapsed_s)).unwrap();
        }

        buffer.close_object().unwrap();

        buffer
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub struct DiscoverComplete {
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub elapsed_s: Option<Elapsed>,
}

impl DiscoverComplete {
    #[cfg(feature = "json")]
    pub fn to_jsonline(&self) -> String {
        use json_write::JsonWrite as _;

        let mut buffer = String::new();
        buffer.open_object().unwrap();

        buffer.key("event").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value("discover_complete").unwrap();

        if let Some(elapsed_s) = self.elapsed_s {
            buffer.val_sep().unwrap();
            buffer.key("elapsed_s").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(String::from(elapsed_s)).unwrap();
        }

        buffer.close_object().unwrap();

        buffer
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub struct RunStart {
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub elapsed_s: Option<Elapsed>,
}

impl RunStart {
    #[cfg(feature = "json")]
    pub fn to_jsonline(&self) -> String {
        use json_write::JsonWrite as _;

        let mut buffer = String::new();
        buffer.open_object().unwrap();

        buffer.key("event").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value("run_start").unwrap();

        if let Some(elapsed_s) = self.elapsed_s {
            buffer.val_sep().unwrap();
            buffer.key("elapsed_s").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(String::from(elapsed_s)).unwrap();
        }

        buffer.close_object().unwrap();

        buffer
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub struct CaseStart {
    pub name: String,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub elapsed_s: Option<Elapsed>,
}

impl CaseStart {
    #[cfg(feature = "json")]
    pub fn to_jsonline(&self) -> String {
        use json_write::JsonWrite as _;

        let mut buffer = String::new();
        buffer.open_object().unwrap();

        buffer.key("event").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value("case_start").unwrap();

        buffer.val_sep().unwrap();
        buffer.key("name").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value(&self.name).unwrap();

        if let Some(elapsed_s) = self.elapsed_s {
            buffer.val_sep().unwrap();
            buffer.key("elapsed_s").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(String::from(elapsed_s)).unwrap();
        }

        buffer.close_object().unwrap();

        buffer
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub struct CaseMessage {
    pub name: String,
    pub kind: MessageKind,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub message: Option<String>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub elapsed_s: Option<Elapsed>,
}

impl CaseMessage {
    #[cfg(feature = "json")]
    pub fn to_jsonline(&self) -> String {
        use json_write::JsonWrite as _;

        let mut buffer = String::new();
        buffer.open_object().unwrap();

        buffer.key("event").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value("case_message").unwrap();

        buffer.val_sep().unwrap();
        buffer.key("name").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value(&self.name).unwrap();

        buffer.val_sep().unwrap();
        buffer.key("kind").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value(self.kind.as_str()).unwrap();

        if let Some(message) = &self.message {
            buffer.val_sep().unwrap();
            buffer.key("message").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(message).unwrap();
        }

        if let Some(elapsed_s) = self.elapsed_s {
            buffer.val_sep().unwrap();
            buffer.key("elapsed_s").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(String::from(elapsed_s)).unwrap();
        }

        buffer.close_object().unwrap();

        buffer
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub struct CaseComplete {
    pub name: String,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub elapsed_s: Option<Elapsed>,
}

impl CaseComplete {
    #[cfg(feature = "json")]
    pub fn to_jsonline(&self) -> String {
        use json_write::JsonWrite as _;

        let mut buffer = String::new();
        buffer.open_object().unwrap();

        buffer.key("event").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value("case_complete").unwrap();

        buffer.val_sep().unwrap();
        buffer.key("name").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value(&self.name).unwrap();

        if let Some(elapsed_s) = self.elapsed_s {
            buffer.val_sep().unwrap();
            buffer.key("elapsed_s").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(String::from(elapsed_s)).unwrap();
        }

        buffer.close_object().unwrap();

        buffer
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub struct RunComplete {
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub elapsed_s: Option<Elapsed>,
}

impl RunComplete {
    #[cfg(feature = "json")]
    pub fn to_jsonline(&self) -> String {
        use json_write::JsonWrite as _;

        let mut buffer = String::new();
        buffer.open_object().unwrap();

        buffer.key("event").unwrap();
        buffer.keyval_sep().unwrap();
        buffer.value("run_complete").unwrap();

        if let Some(elapsed_s) = self.elapsed_s {
            buffer.val_sep().unwrap();
            buffer.key("elapsed_s").unwrap();
            buffer.keyval_sep().unwrap();
            buffer.value(String::from(elapsed_s)).unwrap();
        }

        buffer.close_object().unwrap();

        buffer
    }
}

#[cfg(feature = "serde")]
fn true_default() -> bool {
    true
}

#[cfg(feature = "serde")]
fn is_true(yes: &bool) -> bool {
    *yes
}

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum RunMode {
    #[default]
    Test,
    Bench,
}

impl RunMode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Test => "test",
            Self::Bench => "bench",
        }
    }

    #[cfg(any(feature = "serde", feature = "json"))]
    fn is_default(&self) -> bool {
        *self == Default::default()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MessageKind {
    // Highest precedent items for determining test status last
    Error,
    Ignored,
}

impl MessageKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Error => "error",
            Self::Ignored => "ignored",
        }
    }
}

/// Time elapsed since process start
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "unstable-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(into = "String"))]
#[cfg_attr(feature = "serde", serde(try_from = "String"))]
pub struct Elapsed(pub std::time::Duration);

impl std::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.3}s", self.0.as_secs_f64())
    }
}

impl std::str::FromStr for Elapsed {
    type Err = std::num::ParseFloatError;

    fn from_str(src: &str) -> Result<Self, Self::Err> {
        let secs = src.parse()?;
        Ok(Elapsed(std::time::Duration::from_secs_f64(secs)))
    }
}

impl TryFrom<String> for Elapsed {
    type Error = std::num::ParseFloatError;

    fn try_from(inner: String) -> Result<Self, Self::Error> {
        inner.parse()
    }
}

impl From<Elapsed> for String {
    fn from(elapsed: Elapsed) -> Self {
        elapsed.0.as_secs_f64().to_string()
    }
}

#[cfg(feature = "unstable-schema")]
#[test]
fn dump_event_schema() {
    let schema = schemars::schema_for!(Event);
    let dump = serde_json::to_string_pretty(&schema).unwrap();
    snapbox::assert_data_eq!(dump, snapbox::file!("../event.schema.json").raw());
}
