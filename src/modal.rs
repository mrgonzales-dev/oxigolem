/// What a modal is showing.
pub enum Kind {
    Palette,
    Providers,
    /// Fetching the model list on a worker thread — keeps the UI
    /// responsive when the provider is slow or down. `cached` carries
    /// the last known list so the picker can offer it if the fetch fails.
    ModelsLoading {
        provider: usize,
        cached: Option<Vec<String>>,
    },
    Models {
        provider: usize,
        models: Vec<String>,
    },
    Message(String),
}

pub struct Modal {
    pub kind: Kind,
    pub filter: String,
    pub sel: usize,
}

impl Modal {
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            filter: String::new(),
            sel: 0,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            Kind::Palette => "commands",
            Kind::Providers => "connect provider",
            Kind::ModelsLoading { .. } => "select model",
            Kind::Models { .. } => "select model",
            Kind::Message(_) => "notice",
        }
    }
}

/// A live provider + model choice.
pub struct Connection {
    pub provider: &'static str,
    pub model: String,
}
