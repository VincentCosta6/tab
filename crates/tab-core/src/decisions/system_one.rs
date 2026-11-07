//! TypeSafe's System One format, spoken by TypeSafe and by OpenRouter's
//! `/api/v1/systemone`.
//!
//! Request: `{model, state, questions: {<id>: {type, instructions, criteria}}}`,
//! where a choice's `criteria` maps each option to its description and a
//! score's lists its levels lowest first. Yes/no is `noul`.
//!
//! Answers come back keyed by the same ids: a choice as `{choice,
//! confidence, probabilities: {option: p}}`, a score as `{score, confidence,
//! probabilities: {"0": p, ...}}`, a noul as `{noul}`.

use serde_json::{Map, Value, json};

use super::{Answer, Question, QuestionKind, Usage};

pub(super) fn request(model: &str, state: &Value, questions: &[Question]) -> Value {
    let mut qs = Map::new();
    for q in questions {
        let v = match &q.kind {
            QuestionKind::Choice { options } => json!({
                "type": "choice",
                "instructions": q.instructions,
                "criteria": options.iter().map(|o| (o.value.clone(), Value::from(o.description.clone()))).collect::<Map<_, _>>(),
            }),
            QuestionKind::Score { levels } => json!({
                "type": "score",
                "instructions": q.instructions,
                "criteria": levels.iter().map(|l| l.description.clone()).collect::<Vec<_>>(),
            }),
            QuestionKind::YesNo => json!({ "type": "noul", "instructions": q.instructions }),
        };
        qs.insert(q.id.clone(), v);
    }
    json!({ "model": model, "state": state, "questions": qs })
}

pub(super) fn response(
    v: &Value,
    questions: &[Question],
) -> anyhow::Result<(Vec<(String, Answer)>, Usage)> {
    let answers = v["answers"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("no answers in the response"))?;
    let mut out = Vec::new();
    for q in questions {
        let Some(a) = answers.get(&q.id) else {
            out.push((q.id.clone(), Answer::Refused));
            continue;
        };
        let confidence = a["confidence"].as_f64();
        let answer = match &q.kind {
            QuestionKind::Choice { options } => match a["choice"].as_str() {
                Some(c) if options.iter().any(|o| o.value == c) => Answer::Choice {
                    choice: c.to_string(),
                    confidence,
                    probabilities: options
                        .iter()
                        .map(|o| {
                            (
                                o.value.clone(),
                                a["probabilities"][&o.value].as_f64().unwrap_or(0.0),
                            )
                        })
                        .collect(),
                },
                _ => Answer::Refused,
            },
            QuestionKind::Score { levels } => match a["score"].as_f64() {
                Some(s) => Answer::Score {
                    score: s.clamp(0.0, (levels.len().max(1) - 1) as f64),
                    confidence,
                    probabilities: levels
                        .iter()
                        .enumerate()
                        .map(|(i, l)| {
                            (
                                l.value.clone(),
                                a["probabilities"][i.to_string()].as_f64().unwrap_or(0.0),
                            )
                        })
                        .collect(),
                },
                None => Answer::Refused,
            },
            QuestionKind::YesNo => match a["noul"].as_f64() {
                Some(p) => Answer::YesNo {
                    probability: p.clamp(0.0, 1.0),
                },
                None => Answer::Refused,
            },
        };
        out.push((q.id.clone(), answer));
    }
    let u = &v["usage"];
    let usage = Usage {
        input_tokens: u["input_tokens"].as_i64().unwrap_or(0),
        output_tokens: u["output_tokens"].as_i64().unwrap_or(0),
        cost_usd: u["cost"].as_f64(),
    };
    Ok((out, usage))
}
