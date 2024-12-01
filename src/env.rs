use std::{collections::HashMap, ops::Add};

#[derive(Debug, Clone, Default)]
pub(crate) struct EnvVars(HashMap<String, String>);

impl EnvVars {
    pub(crate) fn new(env: &[(&str, &str)]) -> Self {
        env.into()
    }

    pub(crate) fn empty() -> Self {
        Self(Default::default())
    }
}

impl IntoIterator for EnvVars {
    type Item = String;

    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        let vec: Vec<_> = self.into();
        vec.into_iter()
    }
}

impl From<EnvVars> for HashMap<String, String> {
    fn from(value: EnvVars) -> Self {
        value.0
    }
}

impl From<HashMap<String, String>> for EnvVars {
    fn from(value: HashMap<String, String>) -> Self {
        Self(value)
    }
}

impl From<&[(&str, &str)]> for EnvVars {
    fn from(value: &[(&str, &str)]) -> Self {
        Self(
            value
                .into_iter()
                .map(|&(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        )
    }
}

impl From<EnvVars> for Vec<String> {
    fn from(value: EnvVars) -> Self {
        value
            .0
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect()
    }
}

impl From<&str> for EnvVars {
    fn from(value: &str) -> Self {
        value
            .split("\n")
            .map(|line| line.trim())
            .filter(|&line| line != "")
            .filter_map(parse_env)
            .collect::<HashMap<String, String>>()
            .into()
    }
}

impl From<String> for EnvVars {
    fn from(value: String) -> Self {
        EnvVars::from(value.as_str())
    }
}

fn parse_env(env: &str) -> Option<(String, String)> {
    let tuple: Vec<_> = env.split("=").collect();
    match tuple[..] {
        [name, value] => Some((name.to_owned(), value.to_owned())),
        _ => None,
    }
}

impl Add for EnvVars {
    type Output = Self;

    // TODO: make sure this overwrites the conflicting values on the old one
    fn add(self, other: Self) -> Self {
        Self(self.0.into_iter().chain(other.0).collect())
    }
}
