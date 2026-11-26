use std::borrow::Cow;
use std::path::PathBuf;

use crate::task::{AgentTask, Answer, TaskSpec};
use crate::{Rejection, TaskError};

/// A task given by its parts: label, spec, prompts, images and a decoding
/// closure. Most tasks are this; a task with logic of its own (prompt
/// building that can refuse, several inputs) implements [`AgentTask`].
pub struct PromptTask<'a, O, D>
where
    D: Fn(Answer) -> Result<O, Rejection> + Send + Sync,
{
    label: Cow<'a, str>,
    spec: TaskSpec,
    system_prompt: Cow<'a, str>,
    user_prompt: Cow<'a, str>,
    images: Vec<PathBuf>,
    decode: D,
}

impl<'a, O, D> PromptTask<'a, O, D>
where
    D: Fn(Answer) -> Result<O, Rejection> + Send + Sync,
{
    pub fn new(
        label: impl Into<Cow<'a, str>>,
        spec: TaskSpec,
        system_prompt: impl Into<Cow<'a, str>>,
        user_prompt: impl Into<Cow<'a, str>>,
        decode: D,
    ) -> Self {
        Self {
            label: label.into(),
            spec,
            system_prompt: system_prompt.into(),
            user_prompt: user_prompt.into(),
            images: Vec::new(),
            decode,
        }
    }

    pub fn with_images(mut self, images: Vec<PathBuf>) -> Self {
        self.images = images;
        self
    }
}

impl<O, D> AgentTask for PromptTask<'_, O, D>
where
    O: Send,
    D: Fn(Answer) -> Result<O, Rejection> + Send + Sync,
{
    type Output = O;

    fn label(&self) -> &str {
        &self.label
    }

    fn spec(&self) -> TaskSpec {
        self.spec.clone()
    }

    fn system_prompt(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.system_prompt)
    }

    fn user_prompt(&self) -> Result<Cow<'_, str>, TaskError> {
        Ok(Cow::Borrowed(&self.user_prompt))
    }

    fn images(&self) -> &[PathBuf] {
        &self.images
    }

    fn decode(&self, answer: Answer) -> Result<O, Rejection> {
        (self.decode)(answer)
    }
}

/// The decoding of a task whose caller reads the JSON value itself.
pub fn json_value(answer: Answer) -> Result<serde_json::Value, Rejection> {
    answer.into_json()
}
