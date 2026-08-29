use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Language {
    English,
    Chinese,
}

impl Language {
    pub fn detected() -> Self {
        static LANGUAGE: OnceLock<Language> = OnceLock::new();
        *LANGUAGE.get_or_init(|| {
            let locale = sys_locale::get_locale();
            Self::from_locale(locale.as_deref())
        })
    }

    pub fn from_locale(locale: Option<&str>) -> Self {
        let chinese = locale
            .map(str::trim)
            .and_then(|locale| locale.split(['-', '_']).next())
            .is_some_and(|language| language.eq_ignore_ascii_case("zh"));
        if chinese {
            Self::Chinese
        } else {
            Self::English
        }
    }

    pub const fn text<'a>(self, english: &'a str, chinese: &'a str) -> &'a str {
        match self {
            Self::English => english,
            Self::Chinese => chinese,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Language;

    #[test]
    fn chinese_primary_language_uses_simplified_chinese() {
        for locale in ["zh", "zh-CN", "zh-Hant", "zh_TW", "ZH-cn"] {
            assert_eq!(Language::from_locale(Some(locale)), Language::Chinese);
        }
    }

    #[test]
    fn every_other_or_unknown_locale_uses_english() {
        for locale in [
            None,
            Some(""),
            Some("-zh"),
            Some("zho-CN"),
            Some("en-US"),
            Some("ja-JP"),
        ] {
            assert_eq!(Language::from_locale(locale), Language::English);
        }
    }
}
