use super::*;
use crate::gateway::ModelPrice;
use crate::runtime::{Model, ModelOutput, ModelRequest, RunBudget, RuntimeError};

impl<P: Provider> Model for GatewayTransport<P> {
    fn generate<'a>(
        &'a self,
        request: &'a ModelRequest,
        budget: &'a RunBudget,
    ) -> BoxFuture<'a, Result<ModelOutput, RuntimeError>> {
        Box::pin(async move {
            let timeout = self.gateway.task_profile(&request.label).timeout;
            match tokio::time::timeout(timeout, self.agent_request(request, budget)).await {
                Ok(Ok(output)) => Ok(output),
                Ok(Err(error)) => match budget.snapshot().denial {
                    Some(detail) => Err(RuntimeError::Budget(detail)),
                    None => Err(RuntimeError::Model(error.into())),
                },
                Err(_) => Err(RuntimeError::Model(TaskError::Deadline(timeout))),
            }
        })
    }
}

impl<P: Provider> GatewayTransport<P> {
    async fn agent_request(
        &self,
        request: &ModelRequest,
        budget: &RunBudget,
    ) -> Result<ModelOutput, ModelError> {
        crate::runtime::types::validate_history(&request.messages)
            .map_err(|error| ModelError::misconfigured(error.to_string()))?;
        if request.model.trim().is_empty()
            || !request.temperature.is_finite()
            || request.max_output_tokens == 0
        {
            return Err(ModelError::misconfigured(
                "agent requests need a model, finite temperature and positive output limit",
            ));
        }
        // Count both history and tool schemas in the gateway's input estimate.
        let input = serde_json::to_string(&(&request.messages, &request.tools))
            .map_err(|error| ModelError::misconfigured(error.to_string()))?;
        let mut context = self.gateway.call_context(
            &request.label,
            self.provider.id(),
            &request.model,
            "",
            &input,
            0,
            request.max_output_tokens as usize,
            self.estimator,
        );
        context.transient_attempts = self.transient_attempts;
        context.attempt_timeout = self.attempt_timeout;
        self.gateway.precheck(&context)?;
        let result = self
            .gateway
            .execute_validated(
                &context,
                |model: String| async move {
                    let profile = self.gateway.model_profile(&model);
                    if request.max_output_tokens as usize > profile.max_output
                        || request.max_output_tokens as usize >= profile.context_window
                    {
                        return Err(ModelError::misconfigured(
                            "agent output allowance exceeds the model profile",
                        ));
                    }
                    let limited = budget.snapshot().limit_micros.is_some();
                    let price = self.gateway.config().prices.price(&model);
                    let ceiling = if limited {
                        let ceiling = price
                            .ok_or_else(|| {
                                ModelError::misconfigured(format!("no model price for {model}"))
                            })
                            .and_then(|price| {
                                cost_ceiling(
                                    price,
                                    profile.context_window as u64,
                                    request.max_output_tokens as u64,
                                )
                            });
                        match ceiling {
                            Ok(ceiling) => ceiling,
                            Err(error) => {
                                budget.refuse(error.to_string());
                                return Err(error);
                            }
                        }
                    } else {
                        0
                    };
                    let reservation = budget
                        .reserve(ceiling)
                        .map_err(|error| ModelError::misconfigured(error.to_string()))?;
                    let attempt = ModelRequest {
                        model: model.clone(),
                        ..request.clone()
                    };
                    // Dropping here (failure/timeout/cancellation) keeps the ceiling
                    // charged because a remote attempt may already have incurred cost.
                    let reply = self.provider.converse(&attempt).await?;
                    if let (Some(price), Some(usage)) = (price, reply.usage) {
                        if let (Some(input), Some(output)) =
                            (usage.prompt_tokens, usage.completion_tokens)
                        {
                            match cost_ceiling(price, input, output) {
                                Ok(actual) => reservation.settle(actual),
                                Err(error) if limited => {
                                    budget.refuse(error.to_string());
                                    drop(reservation);
                                }
                                Err(_) => drop(reservation),
                            }
                        }
                        // Missing token fields: reservation drops conservatively.
                    }
                    let turn = match budget.snapshot().denial {
                        Some(detail) => Err(ModelError::misconfigured(detail)),
                        None => reply.turn,
                    };
                    Ok(AttemptOutput::new((model, turn), reply.usage))
                },
                |(model, turn)| {
                    let turn = turn?;
                    turn.validate()
                        .map_err(|error| ModelError::schema_mismatch(error.to_string()))?;
                    Ok(ModelOutput { turn, model })
                },
            )
            .await;
        result
    }
}

/// Reserve the full configured context as input (deliberately conservative),
/// plus the requested output, rounding up so fractional micro-units fit.
fn cost_ceiling(price: &ModelPrice, input: u64, output: u64) -> Result<u64, ModelError> {
    if !price.input_per_million.is_finite()
        || price.input_per_million < 0.0
        || !price.output_per_million.is_finite()
        || price.output_per_million < 0.0
    {
        return Err(ModelError::misconfigured(
            "model prices must be finite and nonnegative",
        ));
    }
    let cost =
        (input as f64 * price.input_per_million + output as f64 * price.output_per_million).ceil();
    if !cost.is_finite() || cost >= u64::MAX as f64 {
        return Err(ModelError::misconfigured(
            "model cost exceeds the accounting range",
        ));
    }
    Ok(cost as u64)
}
