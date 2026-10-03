//! Task recipes: the unit of the Agent Commons.
//!
//! A recipe is a reusable, forkable task definition: a prompt template, the
//! permissions it needs, the environment it runs in, and fixtures that
//! evaluate whether an attempt did the job. Stored as
//! `.gitbots/recipes/<name>/recipe.toml`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Recipe {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Prompt template; `{{input}}` placeholders are filled from `inputs`.
    pub prompt: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, RecipeInput>,
    #[serde(default)]
    pub permissions: RecipePermissions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Environment>,
    #[serde(default)]
    pub evaluation: Evaluation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecipeInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "yes")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

fn yes() -> bool {
    true
}

/// What the recipe asks for. Granted only within the project's mandate.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RecipePermissions {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub network: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Environment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default)]
    pub setup: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evaluation {
    #[serde(default)]
    pub fixtures: Vec<Fixture>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fixture {
    pub name: String,
    pub run: String,
    #[serde(default)]
    pub expect: Expect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    #[default]
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecipeError {
    #[error("recipe name `{0}` must be lowercase letters, digits and dashes")]
    Name(String),
    #[error("prompt uses `{{{{{0}}}}}` but no such input is declared")]
    UndeclaredInput(String),
    #[error("missing required input `{0}`")]
    MissingInput(String),
    #[error("unknown input `{0}`")]
    UnknownInput(String),
    #[error("unterminated `{{{{` in prompt")]
    Unterminated,
    #[error("duplicate fixture name `{0}`")]
    DuplicateFixture(String),
}

impl Recipe {
    /// `name@version`, as recorded on tasks.
    pub fn id(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }

    pub fn validate(&self) -> Result<(), RecipeError> {
        let name_ok = !self.name.is_empty()
            && self.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !name_ok {
            return Err(RecipeError::Name(self.name.clone()));
        }
        for placeholder in placeholders(&self.prompt)? {
            if !self.inputs.contains_key(placeholder) {
                return Err(RecipeError::UndeclaredInput(placeholder.to_owned()));
            }
        }
        let mut seen = BTreeSet::new();
        for f in &self.evaluation.fixtures {
            if !seen.insert(&f.name) {
                return Err(RecipeError::DuplicateFixture(f.name.clone()));
            }
        }
        Ok(())
    }

    /// Fill the prompt template.
    pub fn render(&self, values: &BTreeMap<String, String>) -> Result<String, RecipeError> {
        if let Some(unknown) = values.keys().find(|k| !self.inputs.contains_key(*k)) {
            return Err(RecipeError::UnknownInput(unknown.clone()));
        }
        let mut out = String::with_capacity(self.prompt.len());
        let mut rest = self.prompt.as_str();
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let end = after.find("}}").ok_or(RecipeError::Unterminated)?;
            let key = after[..end].trim();
            let input =
                self.inputs.get(key).ok_or_else(|| RecipeError::UndeclaredInput(key.to_owned()))?;
            match values.get(key).or(input.default.as_ref()) {
                Some(v) => out.push_str(v),
                None if input.required => return Err(RecipeError::MissingInput(key.to_owned())),
                None => {}
            }
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

fn placeholders(template: &str) -> Result<Vec<&str>, RecipeError> {
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or(RecipeError::Unterminated)?;
        found.push(after[..end].trim());
        rest = &after[end + 2..];
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe() -> Recipe {
        serde_json::from_value(serde_json::json!({
            "name": "fix-failing-test",
            "version": "0.1.0",
            "prompt": "Fix {{ test }} in {{crate}}.",
            "inputs": {
                "test": {},
                "crate": {"default": "the workspace"}
            },
            "evaluation": {"fixtures": [{"name": "tests", "run": "cargo test"}]}
        }))
        .unwrap()
    }

    #[test]
    fn renders_with_defaults() {
        let r = recipe();
        r.validate().unwrap();
        let values = BTreeMap::from([("test".to_owned(), "login_works".to_owned())]);
        assert_eq!(r.render(&values).unwrap(), "Fix login_works in the workspace.");
        assert_eq!(r.id(), "fix-failing-test@0.1.0");
    }

    #[test]
    fn errors() {
        let r = recipe();
        assert_eq!(r.render(&BTreeMap::new()), Err(RecipeError::MissingInput("test".into())));
        let bad = BTreeMap::from([("nope".to_owned(), "x".to_owned())]);
        assert_eq!(r.render(&bad), Err(RecipeError::UnknownInput("nope".into())));

        let mut undeclared = recipe();
        undeclared.prompt = "{{ other }}".into();
        assert_eq!(undeclared.validate(), Err(RecipeError::UndeclaredInput("other".into())));
    }
}
