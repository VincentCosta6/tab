//! OpenAI's Decisions API (`POST /v1/decisions`).
//!
//! Request: `{model, input, questions: [{type, name, instructions, ...}]}`
//! with `choice` (`choices: [{value, description}]`), `score`
//! (`levels: [{label, description}]`) and `predicate` (yes/no).
//!
//! Answers come back as an array, matched by `name`: a choice as `{choice,
//! confidence, probabilities: [{value, probability}]}`, a score as `{score,
//! confidence, probabilities: [{value, label, probability}]}`, a predicate
//! as `{probability}`, and `{type: "refusal"}` for a declined question.

use serde_json::{Value, json};

use super::{Answer, Question, QuestionKind, Usage};

pub(super) fn request(model: &str, state: &Value, questions: &[Question]) -> Value {
    let qs: Vec<Value> = questions
        .iter()
        .map(|q| match &q.kind {
            QuestionKind::Choice { options } => json!({
                "type": "choice", "name": q.id, "instructions": q.instructions,
                "choices": options.iter().map(|o| json!({ "value": o.value, "description": o.description })).collect::<Vec<_>>(),
            }),
            QuestionKind::Score { levels } => json!({
                "type": "score", "name": q.id, "instructions": q.instructions,
                "levels": levels.iter().map(|l| json!({ "label": l.value, "description": l.description })).collect::<Vec<_>>(),
            }),
            QuestionKind::YesNo => json!({ "type": "predicate", "name": q.id, "instructions": q.instructions }),
        })
        .collect();
    let input = match state {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    json!({ "model": model, "input": input, "questions": qs })
}

pub(super) fn response(
    v: &Value,
    questions: &[Question],
) -> anyhow::Result<(Vec<(String, Answer)>, Usage)> {
    let answers = v["answers"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("no answers in the response"))?;
    let by_name = |name: &str| answers.iter().find(|a| a["name"] == name);
    // Probabilities as a list of {value|label, probability}, in our order.
    let probs = |a: &Value, values: &[String], key: &str| -> Vec<(String, f64)> {
        let list = a["probabilities"].as_array().cloned().unwrap_or_default();
        values
            .iter()
            .map(|val| {
                let p = list
                    .iter()
                    .find(|e| e[key].as_str() == Some(val) || e["value"].as_str() == Some(val))
                    .and_then(|e| e["probability"].as_f64())
                    .unwrap_or(0.0);
                (val.clone(), p)
            })
            .collect()
    };
    let mut out = Vec::new();
    for q in questions {
        let answer = match by_name(&q.id) {
            None => Answer::Refused,
            Some(a) if a["type"] == "refusal" => Answer::Refused,
            Some(a) => {
                let confidence = a["confidence"].as_f64();
                match &q.kind {
                    QuestionKind::Choice { options } => {
                        let values: Vec<String> = options.iter().map(|o| o.value.clone()).collect();
                        match a["choice"].as_str() {
                            Some(c) if values.iter().any(|v| v == c) => Answer::Choice {
                                choice: c.to_string(),
                                confidence,
                                probabilities: probs(a, &values, "value"),
                            },
                            _ => Answer::Refused,
                        }
                    }
                    QuestionKind::Score { levels } => {
                        let values: Vec<String> = levels.iter().map(|l| l.value.clone()).collect();
                        match a["score"].as_f64() {
                            Some(s) => Answer::Score {
                                score: s.clamp(0.0, (levels.len().max(1) - 1) as f64),
                                confidence,
                                probabilities: probs(a, &values, "label"),
                            },
                            None => Answer::Refused,
                        }
                    }
                    QuestionKind::YesNo => match a["probability"].as_f64() {
                        Some(p) => Answer::YesNo {
                            probability: p.clamp(0.0, 1.0),
                        },
                        None => Answer::Refused,
                    },
                }
            }
        };
        out.push((q.id.clone(), answer));
    }
    let u = &v["usage"];
    let usage = Usage {
        input_tokens: u["input_tokens"].as_i64().unwrap_or(0),
        output_tokens: u["output_tokens"].as_i64().unwrap_or(0),
        cost_usd: None,
    };
    Ok((out, usage))
}
