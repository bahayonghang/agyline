use crate::config::ThemePolarity;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorToken {
    Separator,
    Muted,
    Model,
    DirIcon,
    Dir,
    Git,
    Context,
    Cache,
    Ok,
    Warn,
    Crit,
    Auto,
}

pub fn terminal_sgr(token: ColorToken, polarity: ThemePolarity) -> &'static str {
    match polarity {
        ThemePolarity::Dark => match token {
            ColorToken::Separator => "\x1b[0;37m",
            ColorToken::Muted => "\x1b[0;90m",
            ColorToken::Model => "\x1b[0;96m",
            ColorToken::DirIcon => "\x1b[0;93m",
            ColorToken::Dir => "\x1b[0;92m",
            ColorToken::Git => "\x1b[0;94m",
            ColorToken::Context => "\x1b[0;95m",
            ColorToken::Cache => "\x1b[0;34m",
            ColorToken::Ok => "\x1b[0;92m",
            ColorToken::Warn => "\x1b[0;33m",
            ColorToken::Crit => "\x1b[0;31m",
            ColorToken::Auto => "\x1b[0;91m",
        },
        ThemePolarity::Light => match token {
            ColorToken::Separator => "\x1b[0;90m",
            ColorToken::Muted => "\x1b[0;90m",
            ColorToken::Model => "\x1b[0;36m",
            ColorToken::DirIcon => "\x1b[0;33m",
            ColorToken::Dir => "\x1b[0;32m",
            ColorToken::Git => "\x1b[0;34m",
            ColorToken::Context => "\x1b[0;35m",
            ColorToken::Cache => "\x1b[0;34m",
            ColorToken::Ok => "\x1b[0;32m",
            ColorToken::Warn => "\x1b[0;33m",
            ColorToken::Crit => "\x1b[0;31m",
            ColorToken::Auto => "\x1b[0;31m",
        },
    }
}
