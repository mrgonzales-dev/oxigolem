/// What a modal is showing.
pub enum Kind {
    Palette,
    Providers,
    Models { provider: usize, models: Vec<String> },
    Message(String),
}

pub struct Modal {
    pub kind: Kind,
    pub filter: String,
    pub sel: usize,
}

impl Modal {
    pub fn new(kind: Kind) -> Self {
        Self { kind, filter: String::new(), sel: 0 }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            Kind::Palette => "commands",
            Kind::Providers => "connect provider",
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
