//! The language of the messages taskloom itself writes (budget refusals,
//! breaker errors, deadline and configuration errors, the plain-text JSON
//! fallback instruction sent to the model).

/// Message language.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Locale {
    #[default]
    English,
    /// Simplified Chinese.
    Chinese,
}

impl Locale {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn budget_refused(
        self,
        task: &str,
        model: &str,
        estimated: usize,
        limit: usize,
        budget: usize,
        window: usize,
        output: usize,
    ) -> String {
        match self {
            Self::English => format!(
                "{task}: estimated input of about {estimated} tokens exceeds the budget of {limit} tokens for model {model} (task budget {budget}, model window {window} − reserved output {output}); the request was not sent, reduce the context"
            ),
            Self::Chinese => format!(
                "{task} 预计输入约 {estimated} tokens，超过模型 {model} 的本次预算 {limit} tokens（任务预算 {budget}，模型窗口 {window} − 预留输出 {output}），请求未发送，需要压缩上下文"
            ),
        }
    }

    pub(crate) fn circuit_open(
        self,
        marker: &str,
        provider: &str,
        model: &str,
        waited_secs: u64,
    ) -> String {
        match self {
            Self::English => format!(
                "{marker}: model {model} at {provider} failed repeatedly and its breaker is open (gateway failed repeatedly); still not recovered after waiting {waited_secs}s, retry later"
            ),
            Self::Chinese => format!(
                "{marker}: AI 网关 {provider} 的模型 {model} 连续失败已熔断（gateway failed repeatedly），等待 {waited_secs} 秒后仍未恢复，请稍后重试"
            ),
        }
    }

    pub(crate) fn no_model_available(self, task: &str) -> String {
        match self {
            Self::English => format!("{task}: no usable model to send the request to"),
            Self::Chinese => format!("{task} 没有可用的模型可以发送请求"),
        }
    }

    pub(crate) fn model_not_configured(self) -> &'static str {
        match self {
            Self::English => "configure a model, API base URL and key first",
            Self::Chinese => "请先在设置中配置模型、API 地址和密钥",
        }
    }

    pub(crate) fn json_text_fallback(self) -> &'static str {
        match self {
            Self::English => "Structured output is unavailable. Still return one complete JSON object with the fields above, without Markdown or explanations.",
            Self::Chinese => "结构化输出不可用。请仍然按上面的字段返回一个完整 JSON 对象，不要 Markdown 或解释。",
        }
    }

    pub(crate) fn repair(self, reason: &str, previous: &str) -> String {
        match self {
            Self::English => format!(
                "Your previous answer was not usable: {reason}\n\nPrevious answer:\n{previous}\n\nAnswer again: one complete, corrected answer in the required format."
            ),
            Self::Chinese => format!(
                "你上一次的回答不可用：{reason}\n\n上一次的回答：\n{previous}\n\n请重新回答：按要求的格式给出一个完整、改正后的回答。"
            ),
        }
    }
}
